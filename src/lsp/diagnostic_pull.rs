//! `textDocument/diagnostic` — pull-model diagnostic handler.
//!
//! The push pipeline (`publishDiagnostics` fired from
//! `update_document`) is still the primary path; this handler lets
//! clients that speak the 3.17 pull protocol ask for the current
//! diagnostics on demand.
//!
//! We serve from `Server::diagnostics_cache`, populated by
//! `diagnostics::update_document` at the same time the push is sent.
//! That keeps this handler cheap (no re-lex / re-parse) and
//! guarantees push and pull agree.

use lsp_types::{
    DocumentDiagnosticParams, DocumentDiagnosticReport, DocumentDiagnosticReportResult,
    FullDocumentDiagnosticReport, RelatedFullDocumentDiagnosticReport,
};

use super::Server;

impl Server {
    pub(super) fn document_diagnostic(
        &self,
        params: DocumentDiagnosticParams,
    ) -> DocumentDiagnosticReportResult {
        let items = self
            .diagnostics_cache
            .get(&params.text_document.uri)
            .cloned()
            .unwrap_or_default();

        DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(
            RelatedFullDocumentDiagnosticReport {
                related_documents: None,
                full_document_diagnostic_report: FullDocumentDiagnosticReport {
                    result_id: None,
                    items,
                },
            },
        ))
    }
}
