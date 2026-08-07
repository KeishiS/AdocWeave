//! Resumable expansion state machine over include frames.

use super::*;

pub(super) struct PreprocessMachine {
    pub(super) options: PreprocessOptions,
    pub(super) source_map: source_map::SourceMapBuilder,
    pub(super) directives: Vec<Directive>,
    pub(super) notices: Vec<PreprocessNotice>,
    pub(super) state: ExpansionState,
    pub(super) stack: Vec<ExpansionCursor>,
    pub(super) resolved: BTreeMap<String, Option<ResourceDocument>>,
    pub(super) until_cancel_check: usize,
}

pub(super) struct ExpansionCursor {
    lines: Vec<SelectedLine>,
    document: crate::source::SourceDocument,
    frame: IncludeFrame,
    next_line: usize,
    conditions: Vec<bool>,
    attribute_value_through: Option<usize>,
}

impl ExpansionCursor {
    pub(super) fn new(
        lines: Vec<SelectedLine>,
        frame: IncludeFrame,
    ) -> Result<Self, PreprocessError> {
        let source_id = frame.source_id();
        let selected_source = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<String>();
        let document = crate::source::SourceDocument::new(&selected_source).map_err(|_| {
            error(
                PreprocessErrorKind::InternalInvariant,
                source_id.clone(),
                zero_range(),
                "selected source exceeds the supported position range",
            )
        })?;
        if document.lines().len() < lines.len() {
            return Err(error(
                PreprocessErrorKind::InternalInvariant,
                source_id,
                zero_range(),
                "selected source lines do not preserve physical boundaries",
            ));
        }
        Ok(Self {
            lines,
            document,
            frame,
            next_line: 0,
            conditions: Vec::new(),
            attribute_value_through: None,
        })
    }
}

pub(super) struct PendingInclude {
    frame: IncludeFrame,
    source_id: Option<SourceId>,
    range: TextRange,
    target_range: TextRange,
    expanded_target: String,
    target: String,
    attributes: BTreeMap<String, String>,
    optional: bool,
}

pub(super) enum MachineFailure {
    Error(PreprocessError),
    Cancelled,
}

pub(super) enum MachineLookup {
    Resolved(Option<ResourceDocument>),
    Deferred(ResourceRequest),
    Failed(HostResourceError),
}

impl MachineFailure {
    pub(super) fn into_step(self) -> PreprocessStep {
        match self {
            Self::Error(error) => PreprocessStep::Failed(error),
            Self::Cancelled => PreprocessStep::Cancelled,
        }
    }
}

impl From<PreprocessError> for MachineFailure {
    fn from(error: PreprocessError) -> Self {
        Self::Error(error)
    }
}

impl PreprocessMachine {
    pub(super) fn lines(
        &mut self,
        source: &str,
        cancellation: &dyn CancellationCheck,
    ) -> Result<Vec<SelectedLine>, MachineFailure> {
        let mut offset = 0;
        let mut lines = Vec::new();
        for line in source.split_inclusive('\n') {
            let start = offset;
            offset += line.len();
            let line_range = range(start, offset);
            self.check_cancelled(cancellation)?;
            lines.push(SelectedLine {
                text: line.to_owned(),
                range: line_range,
                mapping: SourceMapping::Identity,
            });
        }
        Ok(lines)
    }

    fn prepare_include(
        &mut self,
        include: ParsedDirective,
        frame: &IncludeFrame,
        range: TextRange,
    ) -> Result<PendingInclude, MachineFailure> {
        #[cfg(test)]
        RESUMABLE_INCLUDE_VISITS.with(|visits| visits.set(visits.get().saturating_add(1)));
        let source_id = frame.source_id();
        if frame.depth() >= self.options.max_include_depth {
            return Err(error(
                PreprocessErrorKind::DepthLimit,
                source_id.clone(),
                range,
                "include depth limit exceeded",
            )
            .into());
        }
        if self
            .state
            .register_include(self.options.max_includes)
            .is_err()
        {
            return Err(error(
                PreprocessErrorKind::IncludeLimit,
                source_id,
                range,
                "include count limit exceeded",
            )
            .into());
        }
        self.bump_node(source_id.clone(), range)?;
        let expanded_target = directive::expand_attributes(
            &include.target,
            self.state.attributes(),
            self.state.attribute_limits(),
        );
        let target = resolve_include_target(&expanded_target, frame.base_uri());
        validate_target(&target, &self.options).map_err(|message| {
            error(
                PreprocessErrorKind::UnsafeTarget,
                source_id.clone(),
                range,
                message,
            )
        })?;
        if frame.contains_target(&target) {
            return Err(error(
                PreprocessErrorKind::IncludeCycle,
                source_id,
                range,
                "include cycle detected",
            )
            .into());
        }
        let attributes = parse_attributes(&include.attributes).map_err(|message| {
            error(
                PreprocessErrorKind::InvalidDirective,
                source_id.clone(),
                range,
                message,
            )
        })?;
        let optional = attributes.contains_key("optional");
        if let Some(encoding) = attributes.get("encoding")
            && !encoding.eq_ignore_ascii_case("utf-8")
            && !encoding.eq_ignore_ascii_case("utf8")
        {
            return Err(error(
                PreprocessErrorKind::UnsupportedEncoding,
                source_id,
                range,
                "resource snapshots contain UTF-8 text only",
            )
            .into());
        }
        Ok(PendingInclude {
            frame: frame.clone(),
            source_id,
            range,
            target_range: relative_range(range, include.target_start, include.target_end),
            expanded_target,
            target,
            attributes,
            optional,
        })
    }

    pub(super) fn resolve_pending(
        &mut self,
        pending: PendingInclude,
        document: Option<ResourceDocument>,
        cancellation: &dyn CancellationCheck,
    ) -> Result<Option<ExpansionCursor>, MachineFailure> {
        let PendingInclude {
            frame,
            source_id,
            range,
            target_range,
            expanded_target,
            target,
            attributes,
            optional,
        } = pending;
        self.directives.push(Directive {
            kind: DirectiveKind::Include,
            source_id: source_id.clone(),
            range,
            authored_target: Some(expanded_target.clone()),
            optional,
            target: target.clone(),
            target_range,
            resource_source_id: document.as_ref().map(|document| document.source_id.clone()),
        });
        let Some(document) = document else {
            if optional {
                self.notices.push(PreprocessNotice {
                    kind: PreprocessNoticeKind::OptionalResourceMissing,
                    source_id,
                    range,
                    target,
                });
                return Ok(None);
            }
            return Err(PreprocessError {
                kind: PreprocessErrorKind::MissingResource,
                source_id,
                range,
                requested_target: Some(expanded_target),
                target: Some(target.clone()),
                message: format!("resource snapshot does not contain {target}"),
            }
            .into());
        };
        let selected = select_lines(&document.source, &attributes, cancellation)
            .map_err(|_| MachineFailure::Cancelled)?;
        let remaining_bytes = self.source_map.remaining_bytes();
        let transformed = transform_lines(selected, &attributes, remaining_bytes, cancellation)
            .map_err(|failure| match failure {
                TransformFailure::Cancelled => MachineFailure::Cancelled,
                TransformFailure::ByteLimit => error(
                    PreprocessErrorKind::ByteLimit,
                    source_id.clone(),
                    range,
                    "preprocessor byte limit exceeded",
                )
                .into(),
            })?;
        let child = frame.child(
            target.clone(),
            document.source_id.clone(),
            target_base(&target),
        );
        Ok(Some(ExpansionCursor::new(transformed, child)?))
    }

    pub(super) fn drive(
        mut self,
        resources: &(impl ResourceLookup + ?Sized),
        cancellation: &dyn CancellationCheck,
    ) -> PreprocessStep {
        loop {
            let Some(mut cursor) = self.stack.pop() else {
                return self.finish(cancellation);
            };
            if cursor.next_line >= cursor.lines.len() {
                if !cursor.conditions.is_empty() {
                    return PreprocessStep::Failed(error(
                        PreprocessErrorKind::UnclosedConditional,
                        cursor.frame.source_id(),
                        zero_range(),
                        "conditional directive is not closed",
                    ));
                }
                continue;
            }
            if self.check_cancelled(cancellation).is_err() {
                return PreprocessStep::Cancelled;
            }
            let line_index = cursor.next_line;
            cursor.next_line += 1;
            #[cfg(test)]
            RESUMABLE_LINE_VISITS.with(|visits| visits.set(visits.get().saturating_add(1)));
            let line = cursor.lines[line_index].clone();
            let source_id = cursor.frame.source_id();
            let content = line.text.trim_end_matches(['\r', '\n']);
            let enabled = cursor.conditions.iter().all(|condition| *condition);
            if cursor
                .attribute_value_through
                .is_some_and(|last_line| line_index <= last_line)
            {
                if let Err(failure) = self.bump_node(source_id.clone(), line.range) {
                    return failure.into_step();
                }
                if let Err(failure) =
                    self.append(&line.text, source_id.clone(), line.range, line.mapping)
                {
                    return failure.into_step();
                }
                if cursor.attribute_value_through == Some(line_index) {
                    cursor.attribute_value_through = None;
                }
                self.push_cursor(cursor);
                continue;
            }
            match directive::recognize(content) {
                RecognizedDirective::Conditional(directive) => {
                    if let Err(failure) = self.process_conditional(
                        &mut cursor,
                        directive,
                        &line,
                        content.len(),
                        enabled,
                    ) {
                        return failure.into_step();
                    }
                    self.push_cursor(cursor);
                }
                RecognizedDirective::Include(include) if enabled => {
                    if self.options.enable_includes {
                        let pending = match self.prepare_include(include, &cursor.frame, line.range)
                        {
                            Ok(pending) => pending,
                            Err(failure) => return failure.into_step(),
                        };
                        self.push_cursor(cursor);
                        match self.lookup_resource(&pending, resources) {
                            MachineLookup::Resolved(document) => {
                                match self.resolve_pending(pending, document, cancellation) {
                                    Ok(Some(child)) => self.push_cursor(child),
                                    Ok(None) => {}
                                    Err(failure) => return failure.into_step(),
                                }
                            }
                            MachineLookup::Deferred(request) => {
                                return PreprocessStep::NeedResource(Box::new(
                                    SuspendedPreprocess {
                                        machine: self,
                                        pending,
                                        request,
                                    },
                                ));
                            }
                            MachineLookup::Failed(error) => {
                                return PreprocessStep::HostError(error);
                            }
                        }
                    } else {
                        if let Err(failure) =
                            self.process_unexpanded_include(include, &line, source_id)
                        {
                            return failure.into_step();
                        }
                        self.push_cursor(cursor);
                    }
                }
                RecognizedDirective::Escaped(literal) if enabled => {
                    if let Err(failure) =
                        self.process_escaped(literal, &line, content.len(), source_id)
                    {
                        return failure.into_step();
                    }
                    self.push_cursor(cursor);
                }
                RecognizedDirective::Text if enabled => {
                    if let Err(failure) =
                        self.process_text(&mut cursor, line_index, &line, content, cancellation)
                    {
                        return failure.into_step();
                    }
                    self.push_cursor(cursor);
                }
                RecognizedDirective::Include(_)
                | RecognizedDirective::Escaped(_)
                | RecognizedDirective::Text => self.push_cursor(cursor),
            }
        }
    }

    pub(super) fn push_cursor(&mut self, cursor: ExpansionCursor) {
        self.stack.push(cursor);
    }

    fn lookup_resource(
        &mut self,
        pending: &PendingInclude,
        resources: &(impl ResourceLookup + ?Sized),
    ) -> MachineLookup {
        if let Some(cached) = self.resolved.get(&pending.target) {
            return MachineLookup::Resolved(cached.clone());
        }
        let result = resources.lookup(&pending.target);
        match result {
            ResourceLookupResult::Ready(document) => {
                self.resolved
                    .insert(pending.target.clone(), Some(document.clone()));
                MachineLookup::Resolved(Some(document))
            }
            ResourceLookupResult::Missing => {
                self.resolved.insert(pending.target.clone(), None);
                MachineLookup::Resolved(None)
            }
            ResourceLookupResult::Deferred => MachineLookup::Deferred(ResourceRequest {
                target: pending.target.clone(),
                optional: pending.optional,
                source_id: pending.source_id.clone(),
                range: pending.range,
                correlation: Arc::new(ResourceCorrelation),
            }),
            ResourceLookupResult::Failed(message) => MachineLookup::Failed(HostResourceError {
                kind: HostResourceErrorKind::LoadFailed,
                target: pending.target.clone(),
                message,
            }),
        }
    }

    fn process_conditional(
        &mut self,
        cursor: &mut ExpansionCursor,
        directive: ParsedDirective,
        line: &SelectedLine,
        content_len: usize,
        enabled: bool,
    ) -> Result<(), MachineFailure> {
        let source_id = cursor.frame.source_id();
        self.bump_node(source_id.clone(), line.range)?;
        self.directives.push(Directive {
            kind: directive.kind,
            source_id: source_id.clone(),
            range: line.range,
            authored_target: None,
            optional: false,
            target: directive.target.clone(),
            target_range: relative_range(line.range, directive.target_start, directive.target_end),
            resource_source_id: None,
        });
        match directive::transition(
            &directive,
            enabled,
            self.state.attributes(),
            self.state.attribute_limits(),
        ) {
            ConditionalTransition::Inline { selected } => {
                if selected {
                    let ending = &line.text[content_len..];
                    self.append(
                        &format!("{}{ending}", directive.attributes),
                        source_id,
                        line.range,
                        SourceMapping::WholeOrigin,
                    )?;
                    self.state.finish_directive_output();
                }
            }
            ConditionalTransition::Open { enabled } => cursor.conditions.push(enabled),
            ConditionalTransition::Close => {
                if cursor.conditions.pop().is_none() {
                    return Err(error(
                        PreprocessErrorKind::InvalidDirective,
                        source_id,
                        line.range,
                        "endif has no matching conditional",
                    )
                    .into());
                }
            }
        }
        Ok(())
    }

    fn process_unexpanded_include(
        &mut self,
        include: ParsedDirective,
        line: &SelectedLine,
        source_id: Option<SourceId>,
    ) -> Result<(), MachineFailure> {
        self.bump_node(source_id.clone(), line.range)?;
        let authored_target = directive::expand_attributes(
            &include.target,
            self.state.attributes(),
            self.state.attribute_limits(),
        );
        let optional = parse_attributes(&include.attributes)
            .is_ok_and(|attributes| attributes.contains_key("optional"));
        self.directives.push(Directive {
            kind: DirectiveKind::Include,
            source_id: source_id.clone(),
            range: line.range,
            authored_target: Some(authored_target),
            optional,
            target: include.target,
            target_range: relative_range(line.range, include.target_start, include.target_end),
            resource_source_id: None,
        });
        self.append(&line.text, source_id, line.range, line.mapping)?;
        self.state.finish_directive_output();
        Ok(())
    }

    fn process_escaped(
        &mut self,
        literal: &str,
        line: &SelectedLine,
        content_len: usize,
        source_id: Option<SourceId>,
    ) -> Result<(), MachineFailure> {
        let ending = &line.text[content_len..];
        self.append(
            &format!("{literal}{ending}"),
            source_id,
            line.range,
            SourceMapping::WholeOrigin,
        )?;
        self.state.finish_directive_output();
        Ok(())
    }

    fn process_text(
        &mut self,
        cursor: &mut ExpansionCursor,
        line_index: usize,
        line: &SelectedLine,
        content: &str,
        cancellation: &dyn CancellationCheck,
    ) -> Result<(), MachineFailure> {
        let source_id = cursor.frame.source_id();
        let delimiter = self.state.observe_delimiter(content);
        let mut document_attribute = false;
        self.bump_node(source_id.clone(), line.range)?;
        if self.state.accepts_attribute(delimiter)
            && crate::attributes::parse_line(
                content,
                cursor.document.lines()[line_index]
                    .content_range()
                    .start()
                    .to_usize(),
                cursor.document.lines()[line_index].full_range(),
            )
            .is_some()
        {
            let parsed = crate::attributes::parse_lines(&cursor.document, line_index, &|| {
                cancellation.is_cancelled()
            })
            .map_err(|failure| match failure {
                crate::parser_support::ParseFailure::Cancelled => MachineFailure::Cancelled,
                crate::parser_support::ParseFailure::Position(_)
                | crate::parser_support::ParseFailure::Budget(_)
                | crate::parser_support::ParseFailure::InternalInvariant => error(
                    PreprocessErrorKind::InternalInvariant,
                    source_id.clone(),
                    line.range,
                    "attribute preprocessing failed",
                )
                .into(),
            })?;
            if let Some((occurrence, _, last_line)) = parsed {
                self.state.apply_attribute(&occurrence);
                document_attribute = true;
                if last_line > line_index {
                    cursor.attribute_value_through = Some(last_line);
                }
            }
        }
        self.append(&line.text, source_id, line.range, line.mapping)?;
        self.state.finish_line(document_attribute, content);
        Ok(())
    }

    fn check_cancelled(
        &mut self,
        cancellation: &dyn CancellationCheck,
    ) -> Result<(), MachineFailure> {
        if self.until_cancel_check == 0 {
            self.until_cancel_check = crate::cancellation::CHECKPOINT_INTERVAL - 1;
            if cancellation.is_cancelled() {
                return Err(MachineFailure::Cancelled);
            }
        } else {
            self.until_cancel_check -= 1;
        }
        Ok(())
    }

    fn bump_node(
        &mut self,
        source_id: Option<SourceId>,
        range: TextRange,
    ) -> Result<(), MachineFailure> {
        if self.state.register_node(self.options.max_expanded_nodes) == Err(ExpansionLimit::Nodes) {
            return Err(error(
                PreprocessErrorKind::NodeLimit,
                source_id,
                range,
                "preprocessor node limit exceeded",
            )
            .into());
        }
        Ok(())
    }

    fn append(
        &mut self,
        value: &str,
        source_id: Option<SourceId>,
        origin_range: TextRange,
        mapping: SourceMapping,
    ) -> Result<(), MachineFailure> {
        self.source_map
            .append(value, source_id.clone(), origin_range, mapping)
            .map_err(|build_error| match build_error {
                source_map::SourceMapBuildError::ByteLimit => error(
                    PreprocessErrorKind::ByteLimit,
                    source_id,
                    origin_range,
                    "preprocessor byte limit exceeded",
                ),
                source_map::SourceMapBuildError::SegmentLimit => error(
                    PreprocessErrorKind::SourceMapLimit,
                    source_id,
                    origin_range,
                    "source map segment limit exceeded",
                ),
            })
            .map_err(MachineFailure::from)
    }

    fn finish(mut self, cancellation: &dyn CancellationCheck) -> PreprocessStep {
        if cancellation.is_cancelled() {
            return PreprocessStep::Cancelled;
        }
        let mut checkpoint = CancellationCheckpoint::new(cancellation);
        match self.source_map.finish_cancellable(
            std::mem::take(&mut self.directives),
            std::mem::take(&mut self.notices),
            &mut checkpoint,
        ) {
            Ok(_) if cancellation.is_cancelled() => PreprocessStep::Cancelled,
            Ok(document) => PreprocessStep::Complete(document),
            Err(source_map::SourceMapFinishError::Cancelled) => PreprocessStep::Cancelled,
            Err(source_map::SourceMapFinishError::Invariant) => {
                PreprocessStep::Failed(PreprocessError {
                    kind: PreprocessErrorKind::InternalInvariant,
                    source_id: self.options.source_id.clone(),
                    range: zero_range(),
                    requested_target: None,
                    target: None,
                    message:
                        "source map segments are unsorted, overlapping, or outside expanded source"
                            .to_owned(),
                })
            }
        }
    }
}
