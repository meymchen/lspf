use lspf::types::{
    Diagnostic, DiagnosticSeverity, DidOpenTextDocumentParams, Position, PublishDiagnosticsParams,
    Range,
};
use lspf::{Context, Documents, LanguageServer};

struct Hello {
    documents: Documents,
}

impl Hello {
    fn new() -> Self {
        Self {
            documents: Documents::new(),
        }
    }
}

impl LanguageServer for Hello {
    fn documents(&self) -> &Documents {
        &self.documents
    }

    async fn text_document_did_open(&self, ctx: &Context, params: DidOpenTextDocumentParams) {
        ctx.publish_diagnostics(PublishDiagnosticsParams {
            uri: params.text_document.uri,
            version: Some(params.text_document.version),
            diagnostics: vec![Diagnostic {
                range: Range {
                    start: Position {
                        line: 0,
                        character: 0,
                    },
                    end: Position {
                        line: 0,
                        character: 0,
                    },
                },
                severity: Some(DiagnosticSeverity::INFORMATION),
                source: Some("lspf-hello".into()),
                message: "lspf saw this document open".into(),
                ..Diagnostic::default()
            }],
        });
    }
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    lspf::stdio(Hello::new()).serve().await
}
