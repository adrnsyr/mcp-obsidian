//! Memori sebagai **MCP Resource**.
//!
//! Setiap memori diekspos lewat URI `memory://<project>/<slug>` sehingga klien
//! MCP bisa me-list dan "attach" isinya langsung sebagai konteks (tanpa harus
//! memanggil tool). Peta project tersedia di `memory://<project>/_MOC`.
//!
//! Modul ini hanya menangani konstruksi & parsing URI; pembacaan file dilakukan
//! oleh pemanggil (server) lewat `Config`.

/// Skema URI resource.
pub const SCHEME: &str = "memory://";

/// MIME type untuk seluruh resource memori.
pub const MIME_MARKDOWN: &str = "text/markdown";

/// Bangun URI resource untuk sebuah memori (atau `_MOC`).
pub fn uri_for(project: &str, slug: &str) -> String {
    format!("{SCHEME}{project}/{slug}")
}

/// Hasil parse URI: project + slug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRef {
    pub project: String,
    pub slug: String,
}

/// Parse `memory://<project>/<slug>` menjadi `(project, slug)`.
///
/// Mengembalikan `None` bila skema salah, atau project/slug kosong. Slug boleh
/// memuat `/`? Tidak — nama memori adalah slug datar, jadi hanya split pertama
/// yang dipakai sebagai project dan sisanya sebagai slug (tanpa `/`).
pub fn parse_uri(uri: &str) -> Option<ResourceRef> {
    let rest = uri.strip_prefix(SCHEME)?;
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
}
