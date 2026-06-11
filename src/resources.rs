//! Memori & dokumen sebagai **MCP Resource**.
//!
//! Setiap memori diekspos lewat URI `memory://<project>/<slug>` (peta di
//! `memory://<project>/_MOC`), dan setiap dokumen lewat `docs://<project>/<slug>`
//! (indeks di `docs://<project>/_DOCS`), sehingga klien MCP bisa me-list dan
//! "attach" isinya langsung sebagai konteks tanpa memanggil tool.
//!
//! Modul ini hanya menangani konstruksi & parsing URI; pembacaan file dilakukan
//! oleh pemanggil (server) lewat `Config`.

/// Skema URI resource memori.
pub const SCHEME: &str = "memory://";

/// Skema URI resource dokumen.
pub const DOCS_SCHEME: &str = "docs://";

/// MIME type untuk seluruh resource (memori & dokumen).
pub const MIME_MARKDOWN: &str = "text/markdown";

/// Bangun URI resource untuk sebuah memori (atau `_MOC`).
pub fn uri_for(project: &str, slug: &str) -> String {
    format!("{SCHEME}{project}/{slug}")
}

/// Bangun URI resource untuk sebuah dokumen (atau `_DOCS`).
pub fn docs_uri_for(project: &str, slug: &str) -> String {
    format!("{DOCS_SCHEME}{project}/{slug}")
}

/// Hasil parse URI: project + slug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRef {
    pub project: String,
    pub slug: String,
}

/// Parse `memory://<project>/<slug>` menjadi `(project, slug)`.
pub fn parse_uri(uri: &str) -> Option<ResourceRef> {
    parse_scheme(uri, SCHEME)
}

/// Parse `docs://<project>/<slug>` menjadi `(project, slug)`.
pub fn parse_docs_uri(uri: &str) -> Option<ResourceRef> {
    parse_scheme(uri, DOCS_SCHEME)
}

/// Parse `<scheme><project>/<slug>`. Mengembalikan `None` bila skema salah, atau
/// project/slug kosong. Slug adalah nama datar — tak boleh memuat `/`.
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
        assert!(parse_uri("memory://demo").is_none()); // tak ada slug
        assert!(parse_uri("memory://demo/").is_none()); // slug kosong
        assert!(parse_uri("memory:///slug").is_none()); // project kosong
        assert!(parse_uri("memory://demo/a/b").is_none()); // slug bersarang
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
        // indeks dokumen
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
