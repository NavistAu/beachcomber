//! The one output-format vocabulary.
//!
//! Every command that prints a value or rows takes `-f` as a format name or
//! a template, and each declares the subset of [`Preset`]s it supports
//! ([`Formats`]). A value is a template only when it holds `{{` or `{%`;
//! anything else must name a format the command has, or it is refused — never
//! silently read as some other format. The suffix on a command (`g.j`,
//! `s.t`, `w.f TPL`) is the short spelling of `-f`, rewritten before clap
//! parses argv ([`rewrite_suffix`]), and is refused on a command without
//! `-f` or without that format.

use libbeachcomber::render::Format as ValueFormat;

/// A named output format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    /// A value as bare text, an object as sorted `address=value` lines.
    Text,
    /// Rows as a coloured, aligned table.
    Human,
    /// Rows as a plain aligned table.
    Table,
    Json,
    /// Shell assignments.
    Sh,
    /// Comma-separated, with no header row.
    Csv,
    /// Comma-separated, with a header row.
    CsvHeader,
    /// Tab-separated, with no header row.
    Tsv,
    /// Tab-separated, with a header row.
    TsvHeader,
}

impl Preset {
    /// Every preset, in the order help lists them.
    const ALL: [Preset; 9] = [
        Preset::Text,
        Preset::Human,
        Preset::Table,
        Preset::Json,
        Preset::Sh,
        Preset::Csv,
        Preset::CsvHeader,
        Preset::Tsv,
        Preset::TsvHeader,
    ];

    /// The name `-f` takes.
    pub fn name(self) -> &'static str {
        match self {
            Preset::Text => "text",
            Preset::Human => "human",
            Preset::Table => "table",
            Preset::Json => "json",
            Preset::Sh => "sh",
            Preset::Csv => "csv",
            Preset::CsvHeader => "CSV",
            Preset::Tsv => "tsv",
            Preset::TsvHeader => "TSV",
        }
    }

    /// The command suffix that spells it, if any: the default formats have
    /// none, since they need no spelling.
    fn suffix(self) -> Option<&'static str> {
        match self {
            Preset::Json => Some("j"),
            Preset::Sh => Some("s"),
            Preset::Csv => Some("c"),
            Preset::CsvHeader => Some("C"),
            Preset::Tsv => Some("t"),
            Preset::TsvHeader => Some("T"),
            Preset::Text | Preset::Human | Preset::Table => None,
        }
    }

    fn from_name(name: &str) -> Option<Preset> {
        Preset::ALL.into_iter().find(|p| p.name() == name)
    }

    /// The value format this preset prints a value in — every preset but the
    /// two tables, which lay out rows.
    pub fn value_format(self) -> Option<ValueFormat> {
        ValueFormat::from_name(self.name())
    }
}

/// What `-f` asked for: a named format, or a template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Format {
    Preset(Preset),
    Template(String),
}

impl Format {
    /// The preset, when this is one.
    pub fn preset(&self) -> Option<Preset> {
        match self {
            Format::Preset(p) => Some(*p),
            Format::Template(_) => None,
        }
    }
}

/// Whether `s` is a template rather than a format name.
pub fn is_template(s: &str) -> bool {
    s.contains("{{") || s.contains("{%")
}

/// The formats one command supports.
pub struct Formats {
    /// The command as the user names it in a message.
    pub command: &'static str,
    /// Its presets, the first being the default.
    pub presets: &'static [Preset],
    /// Whether `-f` also takes a template.
    pub templates: bool,
}

/// `get`: a value, as text, json, shell assignments, csv or tsv, or a template.
pub const GET: Formats = Formats {
    command: "get",
    presets: &[
        Preset::Text,
        Preset::Json,
        Preset::Sh,
        Preset::Csv,
        Preset::CsvHeader,
        Preset::Tsv,
        Preset::TsvHeader,
    ],
    templates: true,
};

/// `watch`: each frame as `get` prints that value.
pub const WATCH: Formats = Formats {
    command: "watch",
    presets: GET.presets,
    templates: true,
};

/// `eval`: the value, as text, json or shell assignments.
pub const EVAL: Formats = Formats {
    command: "eval",
    presets: &[Preset::Text, Preset::Json, Preset::Sh],
    templates: false,
};

/// `status`: rows.
pub const STATUS: Formats = Formats {
    command: "status",
    presets: &[
        Preset::Human,
        Preset::Table,
        Preset::Json,
        Preset::Sh,
        Preset::Csv,
        Preset::CsvHeader,
        Preset::Tsv,
        Preset::TsvHeader,
    ],
    templates: true,
};

/// `check config --origins`.
pub const ORIGINS: Formats = Formats {
    command: "check config --origins",
    presets: &[Preset::Human, Preset::Json],
    templates: false,
};

impl Formats {
    /// The default format.
    pub fn default_preset(&self) -> Preset {
        self.presets[0]
    }

    /// Read a `-f` value.
    pub fn parse(&self, value: &str) -> Result<Format, String> {
        if self.templates && is_template(value) {
            return Ok(Format::Template(value.to_string()));
        }
        match Preset::from_name(value) {
            Some(p) if self.presets.contains(&p) => Ok(Format::Preset(p)),
            _ => Err(format!(
                "{} has no '{value}' format: expected {}",
                self.command,
                self.vocabulary()
            )),
        }
    }

    /// The formats, as help and errors list them: `text (default), json,
    /// …, or a template ({{ … }})`.
    pub fn vocabulary(&self) -> String {
        let mut names: Vec<String> = self
            .presets
            .iter()
            .enumerate()
            .map(|(i, p)| {
                if i == 0 {
                    format!("{} (default)", p.name())
                } else {
                    p.name().to_string()
                }
            })
            .collect();
        if self.templates {
            let last = names.pop().unwrap_or_default();
            names.push(format!("{last}, or a template ({{{{ … }}}})"));
            return names.join(", ");
        }
        match names.len() {
            0 | 1 => names.concat(),
            n => format!("{} or {}", names[..n - 1].join(", "), names[n - 1]),
        }
    }

    /// The `-f` help line.
    pub fn help(&self) -> String {
        format!("Output format: {}", self.vocabulary())
    }
}

/// `-f` for `get`, as clap's value parser.
pub fn get_format(value: &str) -> Result<Format, String> {
    GET.parse(value)
}

/// `-f` for `watch`, as clap's value parser.
pub fn watch_format(value: &str) -> Result<Format, String> {
    WATCH.parse(value)
}

/// `-f` for `eval`, as clap's value parser.
pub fn eval_format(value: &str) -> Result<Format, String> {
    EVAL.parse(value)
}

/// `-f` for `status`, as clap's value parser.
pub fn status_format(value: &str) -> Result<Format, String> {
    STATUS.parse(value)
}

/// `-f` for `check config --origins`, as clap's value parser.
pub fn origins_format(value: &str) -> Result<Format, String> {
    ORIGINS.parse(value)
}

/// What a `-f` template sees beside every name a template reads: the value
/// as `value` (or `values`, in key order, for several keys), and an object's
/// fields as names of their own. A key with no value binds nothing.
pub fn template_locals(
    slots: &[(&str, Option<serde_json::Value>)],
) -> serde_json::Map<String, serde_json::Value> {
    use serde_json::Value;
    let mut locals = serde_json::Map::new();
    for (_, value) in slots {
        if let Some(Value::Object(fields)) = value {
            for (name, field) in fields {
                locals.insert(name.clone(), field.clone());
            }
        }
    }
    match slots {
        [(_, Some(value))] => {
            locals.insert("value".into(), value.clone());
        }
        [_] => {}
        many => {
            let values = many
                .iter()
                .map(|(_, v)| v.clone().unwrap_or(Value::Null))
                .collect();
            locals.insert("values".into(), Value::Array(values));
        }
    }
    locals
}

/// A `-f` template rendered over `slots`, as text: the one rendering of it
/// for `get` and for every frame of a `watch`, so a miss is rendered by the
/// template in both, and an error in it is worded alike.
pub fn render_template(
    template: &str,
    slots: &[(&str, Option<serde_json::Value>)],
    inputs: &libbeachcomber::resolve::Inputs<'_>,
    fetch: &mut dyn libbeachcomber::resolve::Fetch,
) -> Result<String, String> {
    libbeachcomber::resolve::eval_with(template, "-f", inputs, fetch, &template_locals(slots))
        .map(|v| libbeachcomber::render::render_value("", v.as_ref(), ValueFormat::Text))
        .map_err(|e| e.to_string())
}

/// The commands and their aliases, so a suffix on one is recognised as such.
const COMMANDS: &[(&str, &str)] = &[
    ("daemon", "d"),
    ("get", "g"),
    ("status", "s"),
    ("put", "p"),
    ("refresh", "r"),
    ("watch", "w"),
    ("eval", "e"),
    ("init", "i"),
    ("check", "c"),
    ("kill", "k"),
];

/// The formats of a command with `-f`, by the name or alias it was typed as.
fn formats_of(command: &str) -> Option<&'static Formats> {
    match command {
        "get" | "g" => Some(&GET),
        "watch" | "w" => Some(&WATCH),
        "eval" | "e" => Some(&EVAL),
        "status" | "s" => Some(&STATUS),
        _ => None,
    }
}

/// Rewrite a format suffix on the command (`argv[1]`) into `-f`: `g.j` is
/// `g -f json`, and `g.f TPL` is `g -f TPL`. A word that is not a command
/// with a suffix is left for clap. A suffix on a command without `-f`, a
/// suffix that names no format, or one the command does not have is refused.
pub fn rewrite_suffix(mut args: Vec<String>) -> Result<Vec<String>, String> {
    let Some((command, suffix)) = args.get(1).and_then(|a| a.split_once('.')) else {
        return Ok(args);
    };
    let (command, suffix) = (command.to_string(), suffix.to_string());
    if !COMMANDS
        .iter()
        .any(|(name, alias)| command == *name || command == *alias)
    {
        return Ok(args);
    }
    let Some(formats) = formats_of(&command) else {
        return Err(format!(
            "comb {command} takes no format: a format suffix applies to get, watch and status"
        ));
    };
    let value = if suffix == "f" {
        if args.len() < 3 {
            return Err("the .f suffix takes the template that follows it".to_string());
        }
        args.remove(2)
    } else {
        let preset = Preset::ALL
            .into_iter()
            .find(|p| p.suffix() == Some(suffix.as_str()))
            .ok_or_else(|| format!("unknown format suffix '.{suffix}'"))?;
        if !formats.presets.contains(&preset) {
            return Err(format!(
                "{} has no '{}' format: expected {}",
                formats.command,
                preset.name(),
                formats.vocabulary()
            ));
        }
        preset.name().to_string()
    };
    args[1] = command;
    args.insert(2, value);
    args.insert(2, "-f".to_string());
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        std::iter::once("comb")
            .chain(words.iter().copied())
            .map(String::from)
            .collect()
    }

    #[test]
    fn a_template_is_recognised_by_its_tags_never_by_not_being_a_name() {
        assert_eq!(
            GET.parse("[{{ x }}]"),
            Ok(Format::Template("[{{ x }}]".into()))
        );
        assert_eq!(
            GET.parse("{% if x %}y{% endif %}"),
            Ok(Format::Template("{% if x %}y{% endif %}".into()))
        );
        assert!(GET.parse("fmt").unwrap_err().contains("get has no 'fmt'"));
        assert!(
            ORIGINS.parse("{{ x }}").is_err(),
            "origins takes no template"
        );
    }

    #[test]
    fn each_command_takes_only_its_own_presets() {
        assert_eq!(GET.parse("CSV"), Ok(Format::Preset(Preset::CsvHeader)));
        assert!(GET.parse("human").is_err());
        assert_eq!(STATUS.parse("human"), Ok(Format::Preset(Preset::Human)));
        assert!(STATUS.parse("text").is_err());
    }

    #[test]
    fn the_vocabulary_marks_the_default_and_ends_with_templates() {
        assert_eq!(
            GET.vocabulary(),
            "text (default), json, sh, csv, CSV, tsv, TSV, or a template ({{ … }})"
        );
        assert_eq!(ORIGINS.vocabulary(), "human (default) or json");
    }

    #[test]
    fn a_suffix_is_rewritten_to_f() {
        assert_eq!(
            rewrite_suffix(argv(&["g.j", "git"])).unwrap(),
            argv(&["g", "-f", "json", "git"])
        );
        assert_eq!(
            rewrite_suffix(argv(&["status.t"])).unwrap(),
            argv(&["status", "-f", "tsv"])
        );
        assert_eq!(
            rewrite_suffix(argv(&["w.f", "{{ x }}", "git"])).unwrap(),
            argv(&["w", "-f", "{{ x }}", "git"])
        );
    }

    #[test]
    fn a_word_that_is_not_a_command_is_left_for_clap() {
        assert_eq!(
            rewrite_suffix(argv(&["nope.j"])).unwrap(),
            argv(&["nope.j"])
        );
        assert_eq!(rewrite_suffix(argv(&["get"])).unwrap(), argv(&["get"]));
    }

    #[test]
    fn a_suffix_the_command_cannot_take_is_refused() {
        assert!(
            rewrite_suffix(argv(&["p.j"]))
                .unwrap_err()
                .contains("comb p takes no format")
        );
        assert!(
            rewrite_suffix(argv(&["g.p"]))
                .unwrap_err()
                .contains("unknown format suffix '.p'")
        );
        assert!(
            rewrite_suffix(argv(&["s.f"]))
                .unwrap_err()
                .contains("the .f suffix takes the template")
        );
    }
}
