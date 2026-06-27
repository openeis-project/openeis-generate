// post-hook: runs in the DESTINATION dir after expansion. Good for writing
// derived files (they'll be included in the initial git commit).
//
// Hyphenated built-ins like `os-arch` can't be named directly in rhai
// (`os-arch` parses as `os - arch`); read them with `variable::get`.
let os = variable::get("os-arch");
let info = "Rendered by " + username + " (" + os + ") into " + env::destination;
file::write("GENERATED_BY.txt", info);
