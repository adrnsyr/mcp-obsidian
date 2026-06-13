//! Memories & documents as **MCP Resources**.
//!
//! Every memory is exposed via the URI `memory://<project>/<slug>` (the map at
//! `memory://<project>/_MOC`), and every document via `docs://<project>/<slug>`
//! (the index at `docs://<project>/_DOCS`), so an MCP client can list and
//! "attach" their content directly as context without calling a tool.
//!
//! This module only handles URI construction & parsing; reading files is done
//! by the caller (the server) via `Config`.

/// URI scheme for memory resources.
pub const SCHEME: &str = "memory://";

/// URI scheme for document resources.
pub const DOCS_SCHEME: &str = "docs://";

/// MIME type for all resources (memories & documents).
pub const MIME_MARKDOWN: &str = "text/markdown";

/// Build the resource URI for a memory (or `_MOC`).
pub fn uri_for(project: &str, slug: &str) -> String {
    format!("{SCHEME}{project}/{slug}")
}

/// Build the resource URI for a document (or `_DOCS`).
pub fn docs_uri_for(project: &str, slug: &str) -> String {
    format!("{DOCS_SCHEME}{project}/{slug}")
}

/// Result of parsing a URI: project + slug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRef {
    pub project: String,
    pub slug: String,
}

/// Parse `memory://<project>/<slug>` into `(project, slug)`.
pub fn parse_uri(uri: &str) -> Option<ResourceRef> {
    parse_scheme(uri, SCHEME)
}

/// Parse `docs://<project>/<slug>` into `(project, slug)`.
pub fn parse_docs_uri(uri: &str) -> Option<ResourceRef> {
    parse_scheme(uri, DOCS_SCHEME)
}

/// Parse `<scheme><project>/<slug>`. Returns `None` if the scheme is wrong, or
/// the project/slug is empty. The slug is a flat name — it must not contain `/`.
fn parse_scheme(uri: &str, scheme: &str) -> Option<ResourceRef> {
    let rest = uri.strip_prefix(scheme)?;
    let (project, slug) = rest.split_once('/')?;
    if project.is_empty() || slug.is_empty() || slug.contains('/') {
        return None;
    }
    Some(ResourceRef {
        project: project.to_string(),
        slug: slug.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_uri() {
        let uri = uri_for("demo", "auth-flow");
        assert_eq!(uri, "memory://demo/auth-flow");
        assert_eq!(
            parse_uri(&uri),
            Some(ResourceRef {
                project: "demo".into(),
                slug: "auth-flow".into()
            })
        );
    }

    #[test]
    fn rejects_bad_uris() {
        assert!(parse_uri("http://demo/x").is_none());
        assert!(parse_uri("memory://demo").is_none()); // no slug
        assert!(parse_uri("memory://demo/").is_none()); // empty slug
        assert!(parse_uri("memory:///slug").is_none()); // empty project
        assert!(parse_uri("memory://demo/a/b").is_none()); // nested slug
    }

    #[test]
    fn moc_uri_parses() {
        assert_eq!(
            parse_uri("memory://demo/_MOC"),
            Some(ResourceRef {
                project: "demo".into(),
                slug: "_MOC".into()
            })
        );
    }

    #[test]
    fn docs_roundtrip_uri() {
        let uri = docs_uri_for("demo", "login-spec");
        assert_eq!(uri, "docs://demo/login-spec");
        assert_eq!(
            parse_docs_uri(&uri),
            Some(ResourceRef {
                project: "demo".into(),
                slug: "login-spec".into()
            })
        );
        // document index
        assert_eq!(
            parse_docs_uri("docs://demo/_DOCS"),
            Some(ResourceRef {
                project: "demo".into(),
                slug: "_DOCS".into()
            })
        );
    }

    #[test]
    fn schemes_do_not_cross_parse() {
        assert!(parse_docs_uri("memory://demo/x").is_none());
        assert!(parse_uri("docs://demo/x").is_none());
    }
}
