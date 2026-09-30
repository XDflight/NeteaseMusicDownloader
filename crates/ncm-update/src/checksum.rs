/// Find the hash for `file_name` in `sha256sum`-style text (`<hex>  <name>` or `<hex> *<name>`).
pub(crate) fn find_hash(text: &str, file_name: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (hash, name) = line.trim().split_once(char::is_whitespace)?;
        let name = name.trim().trim_start_matches('*');
        let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
        (name == file_name && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit())).then(|| hash.to_ascii_lowercase())
    })
}
