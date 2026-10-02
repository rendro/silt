//! `textDocument/diagnostic` — pull-model diagnostic handler.
//!
//! The push path (`publishDiagnostics` after each analysis) is still
//! the primary one; this handler lets
//! clients that speak the 3.17 pull protocol ask for the current
//! diagnostics on demand.
//!
//! We serve from `Server::published`, the diagnostics last pushed for
//! each file (the scheduled analysis runs before any request). That keeps
//! this handler cheap and guarantees push and pull agree.

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
            .published
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
