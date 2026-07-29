//! Pure Document Symbols projection over an adopted analysis snapshot.

use adocweave::Analysis;
use adocweave::semantic::{
    DocumentSymbol as CoreDocumentSymbol, SymbolKind as CoreSymbolKind, document_symbols,
};
use async_lsp::lsp_types as lsp;

use crate::position::{PositionEncoding, range_to_lsp};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SymbolPresentation {
    Hierarchical,
    Flat,
}

pub(crate) fn symbols(
    analysis: &Analysis,
    uri: &lsp::Url,
    encoding: PositionEncoding,
    presentation: SymbolPresentation,
) -> Result<lsp::DocumentSymbolResponse, String> {
    let symbols = document_symbols(analysis.document());
    match presentation {
        SymbolPresentation::Hierarchical => symbols
            .iter()
            .map(|symbol| nested_symbol(symbol, analysis, encoding))
            .collect::<Result<Vec<_>, _>>()
            .map(lsp::DocumentSymbolResponse::Nested),
        SymbolPresentation::Flat => {
            let mut flat = Vec::new();
            for symbol in &symbols {
                flatten_symbol(symbol, None, analysis, uri, encoding, &mut flat)?;
            }
            Ok(lsp::DocumentSymbolResponse::Flat(flat))
        }
    }
}

#[allow(deprecated)]
fn nested_symbol(
    symbol: &CoreDocumentSymbol,
    analysis: &Analysis,
    encoding: PositionEncoding,
) -> Result<lsp::DocumentSymbol, String> {
    Ok(lsp::DocumentSymbol {
        name: symbol.name.clone(),
        detail: None,
        kind: symbol_kind(symbol.kind),
        tags: None,
        deprecated: None,
        range: range_to_lsp(symbol.range, analysis.source_document(), encoding)?,
        selection_range: range_to_lsp(
            symbol.selection_range,
            analysis.source_document(),
            encoding,
        )?,
        children: Some(
            symbol
                .children
                .iter()
                .map(|child| nested_symbol(child, analysis, encoding))
                .collect::<Result<Vec<_>, _>>()?,
        ),
    })
}

#[allow(deprecated)]
fn flatten_symbol(
    symbol: &CoreDocumentSymbol,
    container_name: Option<&str>,
    analysis: &Analysis,
    uri: &lsp::Url,
    encoding: PositionEncoding,
    output: &mut Vec<lsp::SymbolInformation>,
) -> Result<(), String> {
    output.push(lsp::SymbolInformation {
        name: symbol.name.clone(),
        kind: symbol_kind(symbol.kind),
        tags: None,
        deprecated: None,
        location: lsp::Location::new(
            uri.clone(),
            range_to_lsp(symbol.range, analysis.source_document(), encoding)?,
        ),
        container_name: container_name.map(str::to_owned),
    });
    for child in &symbol.children {
        flatten_symbol(child, Some(&symbol.name), analysis, uri, encoding, output)?;
    }
    Ok(())
}

fn symbol_kind(kind: CoreSymbolKind) -> lsp::SymbolKind {
    match kind {
        CoreSymbolKind::DocumentTitle => lsp::SymbolKind::FILE,
        CoreSymbolKind::Part => lsp::SymbolKind::MODULE,
        CoreSymbolKind::Section => lsp::SymbolKind::NAMESPACE,
        CoreSymbolKind::ListItem => lsp::SymbolKind::STRING,
    }
}

#[cfg(test)]
mod tests {
    use adocweave::{Analysis, AnalysisOptions, AnalysisRequest, NeverCancel};
    use async_lsp::lsp_types as lsp;

    use super::{SymbolPresentation, symbols};
    use crate::PositionEncoding;

    fn analyze(source: &str) -> Analysis {
        AnalysisRequest::new(None, 1, 1, source, AnalysisOptions::default())
            .analyze(&NeverCancel)
            .expect("analysis")
            .analysis
    }

    fn uri() -> lsp::Url {
        "file:///symbols.adoc".parse().expect("valid URI")
    }

    fn nested(source: &str, encoding: PositionEncoding) -> Vec<lsp::DocumentSymbol> {
        match symbols(
            &analyze(source),
            &uri(),
            encoding,
            SymbolPresentation::Hierarchical,
        )
        .expect("document symbols")
        {
            lsp::DocumentSymbolResponse::Nested(symbols) => symbols,
            lsp::DocumentSymbolResponse::Flat(_) => panic!("expected hierarchical symbols"),
        }
    }

    fn flat(source: &str, encoding: PositionEncoding) -> Vec<lsp::SymbolInformation> {
        match symbols(&analyze(source), &uri(), encoding, SymbolPresentation::Flat)
            .expect("document symbols")
        {
            lsp::DocumentSymbolResponse::Flat(symbols) => symbols,
            lsp::DocumentSymbolResponse::Nested(_) => panic!("expected flat symbols"),
        }
    }

    #[test]
    fn hierarchical_symbols_preserve_heading_tree_and_ranges() {
        let symbols = nested(
            "= 題名😀\n\n== 一\n\n=== 子\n\n== 二\n",
            PositionEncoding::Utf16,
        );

        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "題名😀");
        assert_eq!(
            symbols[0].children.as_ref().expect("children")[0].name,
            "一"
        );
        assert_eq!(
            symbols[0].children.as_ref().expect("children")[0]
                .children
                .as_ref()
                .expect("grandchildren")[0]
                .name,
            "子"
        );
        assert_eq!(
            symbols[0].children.as_ref().expect("children")[1].name,
            "二"
        );
        assert_eq!(
            symbols[0].selection_range,
            lsp::Range::new(lsp::Position::new(0, 2), lsp::Position::new(0, 6))
        );
    }

    #[test]
    fn flat_symbols_use_preorder_immediate_containers_and_protocol_kinds() {
        let symbols = flat(
            "= Title\n:doctype: book\n\n= Part One\n\n== Section\n\n* item\n** nested\n",
            PositionEncoding::Utf16,
        );

        assert_eq!(
            symbols
                .iter()
                .map(|symbol| symbol.name.as_str())
                .collect::<Vec<_>>(),
            ["Title", "Part One", "Section", "item", "nested"]
        );
        assert_eq!(
            symbols
                .iter()
                .map(|symbol| symbol.container_name.as_deref())
                .collect::<Vec<_>>(),
            [
                None,
                Some("Title"),
                Some("Part One"),
                Some("Section"),
                Some("item")
            ]
        );
        assert_eq!(
            symbols.iter().map(|symbol| symbol.kind).collect::<Vec<_>>(),
            [
                lsp::SymbolKind::FILE,
                lsp::SymbolKind::MODULE,
                lsp::SymbolKind::NAMESPACE,
                lsp::SymbolKind::STRING,
                lsp::SymbolKind::STRING,
            ]
        );
        assert!(symbols.iter().all(|symbol| symbol.location.uri == uri()));
    }

    #[test]
    fn unicode_ranges_follow_the_negotiated_encoding() {
        let source = "= 題名😀";
        let utf8 = nested(source, PositionEncoding::Utf8);
        let utf16 = nested(source, PositionEncoding::Utf16);

        assert_eq!(utf8[0].range.end.character, 12);
        assert_eq!(utf8[0].selection_range.end.character, 12);
        assert_eq!(utf16[0].range.end.character, 6);
        assert_eq!(utf16[0].selection_range.end.character, 6);
    }
}
