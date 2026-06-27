// pre-hook: runs in the template dir, before rendering. `variable::set` here
// adds/overrides a variable that the templates can then use.
//
// `subject` is a normal (non-hyphenated) name, so we can read it directly.
variable::set("greeting", "Hello, " + to_title_case(subject) + "!");
