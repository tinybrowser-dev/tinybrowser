// https://w3c.github.io/webappsec-csp/#report-violation

use url::Url;

use super::{DialContext, Document, QueuedDial, Task};

impl Document {
    fn json_string(value: &str) -> String {
        use std::fmt::Write as _;
        let mut out = String::with_capacity(value.len() + 2);
        out.push('"');
        for c in value.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => {
                    let _ = write!(out, "\\u{:04x}", c as u32);
                }
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    // https://w3c.github.io/webappsec-csp/#report-violation
    pub(crate) fn fire_csp_violations(
        &mut self,
        target: crate::dom::tree::NodeId,
        violations: &[crate::csp::Violation],
    ) {
        if violations.is_empty() {
            return;
        }
        let mut enriched: Vec<crate::csp::Violation> = violations.to_vec();
        {
            let world = self.world.borrow();
            for violation in &mut enriched {
                if violation.source_file.is_none()
                    && violation.blocked_uri != "inline"
                    && violation.blocked_uri != "eval"
                    && let Some(creator) = world.state.creator_stacks.get(&target)
                {
                    violation.source_file = Some(creator.url.clone());
                    violation.line_number = creator.line;
                    violation.column_number = creator.column;
                }
                if violation.status_code == 0 {
                    violation.status_code = self.csp.http_status;
                }
            }
        }
        self.queue_csp_reports(&enriched);
        self.queue_csp_events(enriched);
    }

    // https://w3c.github.io/webappsec-csp/#report-violation
    pub(super) fn queue_csp_events(&mut self, violations: Vec<crate::csp::Violation>) {
        if self.world.borrow().parser_active {
            self.csp
                .pending_events
                .push_back(Task::CspViolation(violations));
        } else {
            self.tasks.push_back(Task::CspViolation(violations));
        }
    }

    // https://w3c.github.io/webappsec-csp/#report-violation
    pub(super) fn deliver_csp_violations(&mut self, violations: &[crate::csp::Violation]) {
        let Some(document) = self.document_node() else {
            return;
        };
        for violation in violations {
            let target = violation
                .element
                .filter(|element| self.csp_element_target(*element, document))
                .unwrap_or(document);
            self.fire_js(|js| js.fire_csp_violations(target, std::slice::from_ref(violation)));
        }
        self.adopt_js_work();
    }

    // https://w3c.github.io/webappsec-csp/#report-violation
    fn csp_element_target(
        &self,
        element: crate::dom::tree::NodeId,
        document: crate::dom::tree::NodeId,
    ) -> bool {
        if element.document != document.document {
            return false;
        }
        self.world.borrow().document(element).is_some_and(|parsed| {
            crate::dom::tree::is_connected(&parsed.document.base, element.node)
        })
    }

    // https://w3c.github.io/webappsec-csp/#should-navigation-response-to-navigation-request-of-type-in-target-be-blocked-by-content-security-policy
    // https://w3c.github.io/webappsec-csp/#report-violation
    pub(crate) fn queue_csp_reports(&mut self, violations: &[crate::csp::Violation]) {
        for violation in violations {
            // https://w3c.github.io/webappsec-csp/#directive-report-to
            if let Some(url) = self.reporting_url(&violation.report_to) {
                let user_agent = self
                    .world
                    .borrow()
                    .runtime
                    .identity
                    .borrow()
                    .user_agent
                    .as_str()
                    .to_owned();
                let body = Self::reporting_api_body(violation, &user_agent);
                self.queue_report(
                    &url,
                    body,
                    "application/reports+json",
                    crate::protocol::RequestPolicy::CspReportTo,
                );
                continue;
            }
            if violation.report_uris.is_empty() {
                continue;
            }
            // https://w3c.github.io/webappsec-csp/#deprecated-serialize-violation
            let source_fields =
                violation
                    .source_file
                    .as_deref()
                    .map_or_else(String::new, |source| {
                        format!(
                            ",\"source-file\":{},\"line-number\":{},\"column-number\":{}",
                            Self::json_string(&crate::csp::strip_url_for_reports(source)),
                            violation.line_number,
                            violation.column_number,
                        )
                    });
            let body = format!(
                "{{\"csp-report\":{{\"document-uri\":{},\"referrer\":\"\",\"blocked-uri\":{},\"effective-directive\":{},\"violated-directive\":{},\"original-policy\":{},\"disposition\":{},\"status-code\":{},\"script-sample\":{}{}}}}}",
                Self::json_string(&violation.document_uri),
                Self::json_string(&violation.blocked_uri),
                Self::json_string(&violation.effective_directive),
                Self::json_string(&violation.effective_directive),
                Self::json_string(&violation.original_policy),
                Self::json_string(violation.disposition.as_str()),
                violation.status_code,
                Self::json_string(violation.sample.as_deref().unwrap_or_default()),
                source_fields,
            )
            .into_bytes();
            for endpoint in &violation.report_uris {
                let Ok(url) = self.url.join(endpoint) else {
                    continue;
                };
                if !matches!(url.scheme(), "http" | "https") {
                    continue;
                }
                self.queue_report(
                    &url,
                    body.clone(),
                    "application/csp-report",
                    crate::protocol::RequestPolicy::CspReportUri,
                );
            }
        }
    }

    fn reporting_url(&self, names: &[String]) -> Option<Url> {
        for name in names {
            let url = self
                .csp
                .reporting_endpoints
                .iter()
                .find(|(endpoint, _)| endpoint == name)
                .map(|(_, url)| url)?;
            let Ok(url) = self.url.join(url) else {
                continue;
            };
            if matches!(url.scheme(), "http" | "https") {
                return Some(url);
            }
        }
        None
    }

    fn queue_report(
        &mut self,
        url: &Url,
        body: Vec<u8>,
        content_type: &str,
        policy: crate::protocol::RequestPolicy,
    ) {
        let mut dial = QueuedDial::new(DialContext::CspReport, policy, url, &self.dial_initiator());
        dial.req.method = net::Method::POST;
        dial.req.body = body;
        dial.req.content_type = Some(content_type.to_owned());
        self.queue_dial(dial);
    }

    // https://w3c.github.io/webappsec-csp/#cspviolationreportbody
    // https://w3c.github.io/reporting/#send-reports
    fn reporting_api_body(violation: &crate::csp::Violation, user_agent: &str) -> Vec<u8> {
        let sample = violation
            .sample
            .as_deref()
            .map_or_else(|| "null".to_owned(), Self::json_string);
        let source_file = violation.source_file.as_deref().map_or_else(
            || "null".to_owned(),
            |source| Self::json_string(&crate::csp::strip_url_for_reports(source)),
        );
        let (line_number, column_number) = violation.source_file.as_deref().map_or_else(
            || ("null".to_owned(), "null".to_owned()),
            |_| {
                (
                    violation.line_number.to_string(),
                    violation.column_number.to_string(),
                )
            },
        );
        format!(
            "[{{\"type\":\"csp-violation\",\"age\":0,\"url\":{},\"user_agent\":{},\"body\":{{\"documentURL\":{},\"referrer\":\"\",\"blockedURL\":{},\"effectiveDirective\":{},\"originalPolicy\":{},\"sourceFile\":{},\"lineNumber\":{},\"columnNumber\":{},\"sample\":{},\"disposition\":{},\"statusCode\":{}}}}}]",
            Self::json_string(&violation.document_uri),
            Self::json_string(user_agent),
            Self::json_string(&violation.document_uri),
            Self::json_string(&violation.blocked_uri),
            Self::json_string(&violation.effective_directive),
            Self::json_string(&violation.original_policy),
            source_file,
            line_number,
            column_number,
            sample,
            Self::json_string(violation.disposition.as_str()),
            violation.status_code,
        )
        .into_bytes()
    }
}
