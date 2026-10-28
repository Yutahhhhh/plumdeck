//! `mimetypes.guess_type(path)[0]` as Starlette's FileResponse uses it, from
//! a fixed table instead of the OS (deviation D-16).
//!
//! Python read its own defaults and then the system's files: on macOS
//! /etc/apache2/mime.types, on Windows the registry, on Linux
//! /etc/mime.types, so the same file got a different Content-Type per OS.
//! The table is Python 3.12's built-in `types_map` with the audio types of
//! macOS's Apache mime.types laid over it (what the shipped macOS app sent).

/// Python 3.12's `mimetypes.types_map` defaults (strict), by extension.
const DEFAULTS: &[(&str, &str)] = &[
    (".3g2", "audio/3gpp2"),
    (".3gp", "audio/3gpp"),
    (".3gpp", "audio/3gpp"),
    (".3gpp2", "audio/3gpp2"),
    (".a", "application/octet-stream"),
    (".aac", "audio/aac"),
    (".adts", "audio/aac"),
    (".ai", "application/postscript"),
    (".aif", "audio/x-aiff"),
    (".aifc", "audio/x-aiff"),
    (".aiff", "audio/x-aiff"),
    (".ass", "audio/aac"),
    (".au", "audio/basic"),
    (".avi", "video/x-msvideo"),
    (".avif", "image/avif"),
    (".bat", "text/plain"),
    (".bcpio", "application/x-bcpio"),
    (".bin", "application/octet-stream"),
    (".bmp", "image/bmp"),
    (".c", "text/plain"),
    (".cdf", "application/x-netcdf"),
    (".cpio", "application/x-cpio"),
    (".csh", "application/x-csh"),
    (".css", "text/css"),
    (".csv", "text/csv"),
    (".dll", "application/octet-stream"),
    (".doc", "application/msword"),
    (".dot", "application/msword"),
    (".dvi", "application/x-dvi"),
    (".eml", "message/rfc822"),
    (".eps", "application/postscript"),
    (".etx", "text/x-setext"),
    (".exe", "application/octet-stream"),
    (".gif", "image/gif"),
    (".gtar", "application/x-gtar"),
    (".h", "text/plain"),
    (".h5", "application/x-hdf5"),
    (".hdf", "application/x-hdf"),
    (".heic", "image/heic"),
    (".heif", "image/heif"),
    (".htm", "text/html"),
    (".html", "text/html"),
    (".ico", "image/vnd.microsoft.icon"),
    (".ief", "image/ief"),
    (".jpe", "image/jpeg"),
    (".jpeg", "image/jpeg"),
    (".jpg", "image/jpeg"),
    (".js", "text/javascript"),
    (".json", "application/json"),
    (".ksh", "text/plain"),
    (".latex", "application/x-latex"),
    (".loas", "audio/aac"),
    (".m1v", "video/mpeg"),
    (".m3u", "application/vnd.apple.mpegurl"),
    (".m3u8", "application/vnd.apple.mpegurl"),
    (".man", "application/x-troff-man"),
    (".markdown", "text/markdown"),
    (".md", "text/markdown"),
    (".me", "application/x-troff-me"),
    (".mht", "message/rfc822"),
    (".mhtml", "message/rfc822"),
    (".mif", "application/x-mif"),
    (".mjs", "text/javascript"),
    (".mov", "video/quicktime"),
    (".movie", "video/x-sgi-movie"),
    (".mp2", "audio/mpeg"),
    (".mp3", "audio/mpeg"),
    (".mp4", "video/mp4"),
    (".mpa", "video/mpeg"),
    (".mpe", "video/mpeg"),
    (".mpeg", "video/mpeg"),
    (".mpg", "video/mpeg"),
    (".ms", "application/x-troff-ms"),
    (".n3", "text/n3"),
    (".nc", "application/x-netcdf"),
    (".nq", "application/n-quads"),
    (".nt", "application/n-triples"),
    (".nws", "message/rfc822"),
    (".o", "application/octet-stream"),
    (".obj", "application/octet-stream"),
    (".oda", "application/oda"),
    (".opus", "audio/opus"),
    (".p12", "application/x-pkcs12"),
    (".p7c", "application/pkcs7-mime"),
    (".pbm", "image/x-portable-bitmap"),
    (".pdf", "application/pdf"),
    (".pfx", "application/x-pkcs12"),
    (".pgm", "image/x-portable-graymap"),
    (".pl", "text/plain"),
    (".png", "image/png"),
    (".pnm", "image/x-portable-anymap"),
    (".pot", "application/vnd.ms-powerpoint"),
    (".ppa", "application/vnd.ms-powerpoint"),
    (".ppm", "image/x-portable-pixmap"),
    (".pps", "application/vnd.ms-powerpoint"),
    (".ppt", "application/vnd.ms-powerpoint"),
    (".ps", "application/postscript"),
    (".pwz", "application/vnd.ms-powerpoint"),
    (".py", "text/x-python"),
    (".pyc", "application/x-python-code"),
    (".pyo", "application/x-python-code"),
    (".qt", "video/quicktime"),
    (".ra", "audio/x-pn-realaudio"),
    (".ram", "application/x-pn-realaudio"),
    (".ras", "image/x-cmu-raster"),
    (".rdf", "application/xml"),
    (".rgb", "image/x-rgb"),
    (".roff", "application/x-troff"),
    (".rst", "text/x-rst"),
    (".rtx", "text/richtext"),
    (".sgm", "text/x-sgml"),
    (".sgml", "text/x-sgml"),
    (".sh", "application/x-sh"),
    (".shar", "application/x-shar"),
    (".snd", "audio/basic"),
    (".so", "application/octet-stream"),
    (".src", "application/x-wais-source"),
    (".srt", "text/plain"),
    (".sv4cpio", "application/x-sv4cpio"),
    (".sv4crc", "application/x-sv4crc"),
    (".svg", "image/svg+xml"),
    (".swf", "application/x-shockwave-flash"),
    (".t", "application/x-troff"),
    (".tar", "application/x-tar"),
    (".tcl", "application/x-tcl"),
    (".tex", "application/x-tex"),
    (".texi", "application/x-texinfo"),
    (".texinfo", "application/x-texinfo"),
    (".tif", "image/tiff"),
    (".tiff", "image/tiff"),
    (".tr", "application/x-troff"),
    (".trig", "application/trig"),
    (".tsv", "text/tab-separated-values"),
    (".txt", "text/plain"),
    (".ustar", "application/x-ustar"),
    (".vcf", "text/x-vcard"),
    (".vtt", "text/vtt"),
    (".wasm", "application/wasm"),
    (".wav", "audio/x-wav"),
    (".webm", "video/webm"),
    (".webmanifest", "application/manifest+json"),
    (".wiz", "application/msword"),
    (".wsdl", "application/xml"),
    (".xbm", "image/x-xbitmap"),
    (".xlb", "application/vnd.ms-excel"),
    (".xls", "application/vnd.ms-excel"),
    (".xml", "text/xml"),
    (".xpdl", "application/xml"),
    (".xpm", "image/x-xpixmap"),
    (".xsl", "application/xml"),
    (".xwd", "image/x-xwindowdump"),
    (".zip", "application/zip"),
];

/// Audio types from macOS's /etc/apache2/mime.types, which Python loaded
/// over its defaults there.
const MACOS_AUDIO: &[(&str, &str)] = &[
    (".aac", "audio/x-aac"),
    (".aif", "audio/x-aiff"),
    (".aifc", "audio/x-aiff"),
    (".aiff", "audio/x-aiff"),
    (".flac", "audio/x-flac"),
    (".m4a", "audio/mp4"),
    (".mp3", "audio/mpeg"),
    (".oga", "audio/ogg"),
    (".ogg", "audio/ogg"),
    (".wav", "audio/x-wav"),
    (".wma", "audio/x-ms-wma"),
];

/// `encodings_map` (case-sensitive) and `suffix_map`.
const ENCODINGS: &[&str] = &[".gz", ".Z", ".bz2", ".xz", ".br"];
const SUFFIXES: &[(&str, &str)] = &[
    (".svgz", ".svg.gz"),
    (".tgz", ".tar.gz"),
    (".taz", ".tar.gz"),
    (".tz", ".tar.gz"),
    (".tbz2", ".tar.bz2"),
    (".txz", ".tar.xz"),
];

/// `posixpath.splitext`: the last `.suffix` of the final component, unless
/// the component is only dots before it.
fn splitext(path: &str) -> (&str, &str) {
    let name_start = path.rfind('/').map_or(0, |at| at + 1);
    let name = &path[name_start..];
    let Some(dot) = name.rfind('.') else { return (path, "") };
    if name[..dot].chars().all(|c| c == '.') {
        return (path, "");
    }
    path.split_at(name_start + dot)
}

/// The type for an extension: the macOS audio table, then Python's
/// defaults; exact match first, then lowercase, as `guess_type` looks up.
fn lookup(ext: &str) -> Option<&'static str> {
    let find = |key: &str| {
        MACOS_AUDIO.iter().chain(DEFAULTS.iter()).find(|(e, _)| *e == key).map(|(_, mime)| *mime)
    };
    find(ext).or_else(|| find(&ext.to_lowercase()))
}

/// `mimetypes.guess_type(path)[0]` for a filesystem path (no URL scheme).
pub fn guess_type(path: &str) -> Option<&'static str> {
    let (base, ext) = splitext(path);
    let (mut base, mut ext) = (base.to_owned(), ext.to_owned());
    while let Some((_, replacement)) = SUFFIXES.iter().find(|(suffix, _)| *suffix == ext.to_lowercase()) {
        let joined = format!("{base}{replacement}");
        let (b, e) = splitext(&joined);
        (base, ext) = (b.to_owned(), e.to_owned());
    }
    if ENCODINGS.contains(&ext.as_str()) {
        ext = splitext(&base).1.to_owned();
    }
    lookup(&ext)
}

/// Starlette's FileResponse media type: the guess or
/// `application/octet-stream`.
pub fn file_media_type(path: &str) -> &'static str {
    guess_type(path).unwrap_or("application/octet-stream")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_like_python_with_the_macos_audio_table() {
        assert_eq!(guess_type("/m/a.mp3"), Some("audio/mpeg"));
        assert_eq!(guess_type("/m/UPPER.MP3"), Some("audio/mpeg"));
        assert_eq!(guess_type("/m/a.flac"), Some("audio/x-flac"));
        assert_eq!(guess_type("/m/a.m4a"), Some("audio/mp4"));
        assert_eq!(guess_type("/m/a.aac"), Some("audio/x-aac"));
        assert_eq!(guess_type("/m/notes.txt"), Some("text/plain"));
        assert_eq!(guess_type("/m/a.mp3.gz"), Some("audio/mpeg"));
        assert_eq!(guess_type("/m/a.MP3.GZ"), None);
        assert_eq!(guess_type("/m/a.tgz"), Some("application/x-tar"));
        assert_eq!(guess_type("/m/.mp3"), None);
        assert_eq!(guess_type("/m.d/no-extension"), None);
        assert_eq!(file_media_type("/m/x.unknown"), "application/octet-stream");
    }
}
