//! Include directive discovery, line selection, and target resolution.

use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncludeRequest {
    pub range: TextRange,
    pub target_range: TextRange,
    pub target: String,
    pub attributes: String,
}

/// Finds syntactically complete, unescaped include directives without performing I/O.
///
/// Hosts may load a superset of resources from these requests. Conditional evaluation and
/// authoritative target validation remain the responsibility of [`preprocess`].
pub fn discover_includes(source: &str) -> Result<Vec<IncludeRequest>, PositionError> {
    TextSize::new(source.len())?;
    let mut requests = Vec::new();
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        let end = offset + line.len();
        let content = line.trim_end_matches(['\r', '\n']);
        if let RecognizedDirective::Include(include) = directive::recognize(content) {
            requests.push(IncludeRequest {
                range: TextRange::new(TextSize::new(offset)?, TextSize::new(end)?)?,
                target_range: TextRange::new(
                    TextSize::new(offset + include.target_start)?,
                    TextSize::new(offset + include.target_end)?,
                )?,
                target: include.target,
                attributes: include.attributes,
            });
        }
        offset = end;
    }
    Ok(requests)
}

pub(super) fn parse_attributes(value: &str) -> Result<BTreeMap<String, String>, String> {
    let mut fields = Vec::new();
    let mut start = 0;
    let mut quote = None;
    for (index, character) in value.char_indices() {
        match quote {
            Some(active) if character == active => quote = None,
            Some(_) => {}
            None if matches!(character, '\'' | '"') => quote = Some(character),
            None if character == ',' => {
                fields.push(&value[start..index]);
                start = index + 1;
            }
            None => {}
        }
    }
    if quote.is_some() {
        return Err("include attribute list has an unclosed quote".to_owned());
    }
    fields.push(&value[start..]);
    let mut attributes = BTreeMap::new();
    for field in fields {
        let field = field.trim();
        if field.is_empty() {
            continue;
        }
        if let Some((name, value)) = field.split_once('=') {
            let name = name.trim();
            let value = value.trim();
            if name.is_empty() {
                return Err("include attribute name is empty".to_owned());
            }
            let quoted = value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
                .or_else(|| {
                    value
                        .strip_prefix('"')
                        .and_then(|value| value.strip_suffix('"'))
                });
            if (value.starts_with(['\'', '"']) || value.ends_with(['\'', '"'])) && quoted.is_none()
            {
                return Err("include attribute quote is malformed".to_owned());
            }
            attributes.insert(name.to_owned(), quoted.unwrap_or(value).to_owned());
        } else {
            attributes.insert(field.to_owned(), String::new());
        }
    }
    Ok(attributes)
}

#[derive(Clone)]
pub(super) struct SelectedLine {
    pub(super) text: String,
    pub(super) range: TextRange,
    pub(super) mapping: SourceMapping,
}

pub(super) fn select_lines(
    source: &str,
    attributes: &BTreeMap<String, String>,
    cancellation: &dyn CancellationCheck,
) -> Result<Vec<SelectedLine>, TextRange> {
    let mut checkpoint = CancellationCheckpoint::new(cancellation);
    let requested_tags = attributes
        .get("tag")
        .into_iter()
        .chain(attributes.get("tags"))
        .flat_map(|value| value.split([';', ',']))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>();
    let requested_lines = attributes
        .get("lines")
        .map(|value| parse_line_selection(value, cancellation))
        .transpose()?;
    let mut active_tags = Vec::<String>::new();
    let mut offset = 0;
    let mut output = Vec::new();
    for (index, line) in source.split_inclusive('\n').enumerate() {
        let line_range = range(offset, offset + line.len());
        if checkpoint.is_cancelled() {
            return Err(line_range);
        }
        let content = line.trim_end_matches(['\r', '\n']);
        if let Some(tag) = tag_marker(content, "tag::") {
            active_tags.push(tag.to_owned());
            offset += line.len();
            continue;
        }
        if let Some(tag) = tag_marker(content, "end::") {
            if let Some(position) = active_tags.iter().rposition(|active| active == tag) {
                active_tags.remove(position);
            }
            offset += line.len();
            continue;
        }
        let number = index + 1;
        let tag_selected = requested_tags.is_empty()
            || active_tags
                .iter()
                .any(|tag| requested_tags.contains(tag.as_str()));
        let line_selected = requested_lines
            .as_ref()
            .is_none_or(|lines| lines.contains(number));
        if tag_selected && line_selected {
            output.push(SelectedLine {
                text: line.to_owned(),
                range: line_range,
                mapping: SourceMapping::Identity,
            });
        }
        offset += line.len();
    }
    Ok(output)
}

pub(super) fn tag_marker<'a>(value: &'a str, marker: &str) -> Option<&'a str> {
    let offset = value.find(marker)?;
    let rest = &value[offset + marker.len()..];
    rest.strip_suffix("[]")
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct LineSelection {
    pub(super) ranges: Vec<(usize, usize)>,
}

impl LineSelection {
    fn contains(&self, line: usize) -> bool {
        let index = self
            .ranges
            .partition_point(|(_, range_end)| *range_end < line);
        self.ranges
            .get(index)
            .is_some_and(|(range_start, range_end)| *range_start <= line && line <= *range_end)
    }
}

pub(super) fn parse_line_selection(
    value: &str,
    cancellation: &dyn CancellationCheck,
) -> Result<LineSelection, TextRange> {
    let mut checkpoint = CancellationCheckpoint::new(cancellation);
    let mut ranges = BTreeMap::<usize, usize>::new();
    for item in value.split([';', ',']) {
        if checkpoint.is_cancelled() {
            return Err(zero_range());
        }
        if let Some((start, end)) = item.trim().split_once("..") {
            if let (Ok(start), Ok(end)) = (start.parse::<u128>(), end.parse::<u128>())
                && start <= end
                && start <= usize::MAX as u128
            {
                let start = start as usize;
                let end = end.min(usize::MAX as u128) as usize;
                ranges
                    .entry(start)
                    .and_modify(|previous| *previous = (*previous).max(end))
                    .or_insert(end);
            }
        } else if let Ok(line) = item.trim().parse::<u128>()
            && line <= usize::MAX as u128
        {
            let line = line as usize;
            ranges.entry(line).or_insert(line);
        }
    }
    let mut normalized: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        if checkpoint.is_cancelled() {
            return Err(zero_range());
        }
        if let Some((_, previous_end)) = normalized.last_mut()
            && start <= previous_end.saturating_add(1)
        {
            *previous_end = (*previous_end).max(end);
        } else {
            normalized.push((start, end));
        }
    }
    Ok(LineSelection { ranges: normalized })
}

/// Why a selected include body could not be transformed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TransformFailure {
    Cancelled,
    ByteLimit,
}

/// Applies `indent` and `leveloffset` to the selected include body.
///
/// The `indent` attribute comes from the document and grows every selected
/// line, so the padding is charged against the remaining expansion budget
/// before it is allocated. Charging afterwards would let a single directive
/// materialize far more text than `max_total_bytes` permits.
pub(super) fn transform_lines(
    lines: Vec<SelectedLine>,
    attributes: &BTreeMap<String, String>,
    remaining_bytes: usize,
    cancellation: &dyn CancellationCheck,
) -> Result<Vec<SelectedLine>, TransformFailure> {
    let mut checkpoint = CancellationCheckpoint::new(cancellation);
    let indent = attributes
        .get("indent")
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(0);
    let leveloffset = attributes
        .get("leveloffset")
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(0);
    let mut output = Vec::with_capacity(lines.len());
    let mut charged_padding = 0usize;
    for mut line in lines {
        if checkpoint.is_cancelled() {
            return Err(TransformFailure::Cancelled);
        }
        let original = line.text.clone();
        if leveloffset != 0 {
            line.text = apply_leveloffset(&line.text, leveloffset);
        }
        if indent > 0 {
            let padding = indent.unsigned_abs() as usize;
            charged_padding = charged_padding.saturating_add(padding);
            if charged_padding > remaining_bytes {
                return Err(TransformFailure::ByteLimit);
            }
            let mut padded = String::with_capacity(padding.saturating_add(line.text.len()));
            padded.extend(std::iter::repeat_n(' ', padding));
            padded.push_str(&line.text);
            line.text = padded;
        } else if indent < 0 {
            let remove = indent.unsigned_abs() as usize;
            let leading = line
                .text
                .bytes()
                .take_while(|byte| *byte == b' ')
                .count()
                .min(remove);
            line.text.drain(..leading);
        }
        if line.text != original {
            line.mapping = SourceMapping::WholeOrigin;
        }
        output.push(line);
    }
    Ok(output)
}

pub(super) fn apply_leveloffset(line: &str, offset: i32) -> String {
    let marker_count = line.bytes().take_while(|byte| *byte == b'=').count();
    if marker_count == 0 || line.as_bytes().get(marker_count) != Some(&b' ') {
        return line.to_owned();
    }
    let adjusted = i32::try_from(marker_count)
        .unwrap_or(i32::MAX)
        .saturating_add(offset)
        .clamp(1, 6) as usize;
    format!("{}{}", "=".repeat(adjusted), &line[marker_count..])
}

pub(super) fn validate_target(
    target: &str,
    options: &PreprocessOptions,
) -> Result<(), &'static str> {
    if target.is_empty()
        || target.chars().any(|character| character.is_control())
        || target.starts_with('/')
        || target.starts_with('\\')
        || target.contains('\\')
        || target.split('/').any(|segment| segment == "..")
    {
        return Err("unsafe include target");
    }
    if let Some((scheme, _)) = target.split_once(':')
        && (options.safe_mode == SafeMode::Secure
            || !options
                .allowed_schemes
                .contains(&scheme.to_ascii_lowercase()))
    {
        return Err("include target scheme is not allowed");
    }
    Ok(())
}

pub fn resolve_include_target(target: &str, base_uri: Option<&str>) -> String {
    if target.contains(':') || target.starts_with('/') || target.starts_with('\\') {
        return target.to_owned();
    }
    if let Some(base_uri) = base_uri.filter(|base| base.contains(':')) {
        return format!("{}/{target}", base_uri.trim_end_matches('/'));
    }
    let combined = base_uri
        .filter(|base| !base.is_empty())
        .map_or_else(|| target.to_owned(), |base| format!("{base}/{target}"));
    let mut segments = Vec::new();
    for segment in combined.split('/') {
        match segment {
            "" | "." => {}
            ".." if segments.last().is_some_and(|segment| *segment != "..") => {
                segments.pop();
            }
            _ => segments.push(segment),
        }
    }
    segments.join("/")
}

pub(super) fn target_base(target: &str) -> Option<String> {
    target
        .rsplit_once('/')
        .map(|(base, _)| base.to_owned())
        .filter(|base| !base.is_empty())
}
