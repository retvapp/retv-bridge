// Which files of `apps/retv-tizen` go into the release package. Shared by
// build.rs (which bundles them) and the crate (which tests the rule), the way
// `tools/samsung/install.js` picks them without `--dev`.

/// `rel` is the path inside the app folder with `/` separators.
pub fn shipped(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.iter().any(|p| p.starts_with('.')) {
        return false;
    }
    let name = parts.last().copied().unwrap_or("");
    if name == "author-signature.xml" || (name.starts_with("signature") && name.ends_with(".xml")) {
        return false;
    }
    // The developer bridge is only for the developer's own TV.
    if rel == "dev.js" {
        return false;
    }
    // Of the service only the built bundle ships.
    if parts[0] == "service" {
        return parts.len() >= 3 && parts[1] == "dist";
    }
    true
}
