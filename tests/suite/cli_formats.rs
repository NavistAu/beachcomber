//! One output-format vocabulary. Every command that prints a value or rows
//! takes `-f` as a format name or a template (recognised by `{{` or `{%`),
//! from the subset of formats it supports. A format suffix on the command
//! (`g.j`, `s.t`, `w.f TPL`) is the short spelling of `-f`, and applies only
//! to a command that has `-f` and that format.

use crate::common::daemon::TestDaemon;
use std::path::Path;

fn ok(d: &TestDaemon, args: &[&str]) -> String {
    let out = d.comb(Path::new("/")).args(args).assert().success();
    String::from_utf8(out.get_output().stdout.clone()).unwrap()
}

/// `args` fail with exit 2 and stderr holding `needle`.
fn refused(d: &TestDaemon, args: &[&str], needle: &str) {
    d.comb(Path::new("/"))
        .args(args)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(needle));
}

fn seed(d: &TestDaemon) {
    ok(
        d,
        &["put", "--global", "fmtprov", r#"{"val":"formatted","n":3}"#],
    );
}

// ── -f takes a format name or a template ─────────────────────────────────────

#[test]
fn get_takes_a_template_as_the_value_of_f() {
    let d = TestDaemon::spawn();
    seed(&d);
    assert_eq!(
        ok(
            &d,
            &["get", "--global", "-f", "[{{ val }}/{{ n }}]", "fmtprov"]
        ),
        "[formatted/3]"
    );
    // The suffix is the short spelling of the same thing.
    assert_eq!(
        ok(&d, &["g.f", "[{{ val }}/{{ n }}]", "fmtprov", "--global"]),
        "[formatted/3]"
    );
}

#[test]
fn status_takes_a_template_through_the_f_suffix() {
    let d = TestDaemon::spawn();
    seed(&d);
    assert_eq!(
        ok(
            &d,
            &[
                "s.f",
                "<{{ field }}>",
                "--filter",
                "provider=fmtprov",
                "--sort",
                "field"
            ]
        ),
        "<n>\n<val>\n"
    );
}

// ── suffixes ─────────────────────────────────────────────────────────────────

#[test]
fn a_suffix_on_a_command_without_f_is_refused_by_name() {
    let d = TestDaemon::spawn();
    refused(&d, &["p.j", "x", "1"], "comb p takes no format");
    refused(&d, &["k.j"], "comb k takes no format");
}

#[test]
fn an_unknown_suffix_is_refused() {
    let d = TestDaemon::spawn();
    refused(&d, &["g.Z", "x"], "unknown format suffix '.Z'");
    // `.p` was a second spelling of the default; the default needs none.
    refused(&d, &["g.p", "x"], "unknown format suffix '.p'");
}

#[test]
fn the_f_suffix_needs_the_template_after_it() {
    let d = TestDaemon::spawn();
    refused(
        &d,
        &["g.f"],
        "the .f suffix takes the template that follows it",
    );
}

// ── help ─────────────────────────────────────────────────────────────────────

#[test]
fn get_and_watch_list_the_same_formats_in_their_help() {
    let d = TestDaemon::spawn();
    let want = "text (default), json, sh, csv, CSV, tsv, TSV, or a template";
    for command in ["get", "watch"] {
        let help = ok(&d, &[command, "--help"]);
        let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(flat.contains(want), "{command} --help:\n{help}");
    }
}

// ── one encoder per format ───────────────────────────────────────────────────

/// A cell holding a comma or a quote is quoted, so the header and the row
/// have as many cells as each other.
#[test]
fn csv_quotes_its_cells_so_header_and_row_agree() {
    let d = TestDaemon::spawn();
    ok(
        &d,
        &["put", "--global", "dq", r#"{"a":"x,y","b":"say \"hi\""}"#],
    );
    assert_eq!(
        ok(&d, &["g.C", "dq", "--global"]),
        "a,b\n\"x,y\",\"say \"\"hi\"\"\"\n"
    );
}

/// Every `sh` line assigns a shell variable, whatever the address: a nested
/// leaf, a name holding a `.` or a space.
#[test]
fn sh_assigns_a_shell_variable_for_any_address() {
    let d = TestDaemon::spawn();
    ok(
        &d,
        &["put", "--global", "dk", r#"{"a.b":1,"c":{"d":2},"e f":3}"#],
    );
    let out = ok(&d, &["g.s", "dk", "--global"]);
    let shell = std::process::Command::new("sh")
        .args([
            "-c",
            r#"eval "$OUT"; printf '%s|%s|%s' "$a_b" "$c_d" "$e_f""#,
        ])
        .env("OUT", &out)
        .output()
        .expect("sh runs");
    assert_eq!(String::from_utf8(shell.stdout).unwrap(), "1|2|3", "{out}");
}

/// A scalar prints bare for embedding; a container is a document of lines.
#[test]
fn a_scalar_has_no_trailing_newline_and_a_document_ends_in_one() {
    let d = TestDaemon::spawn();
    seed(&d);
    assert_eq!(ok(&d, &["get", "--global", "fmtprov.val"]), "formatted");
    assert_eq!(
        ok(&d, &["get", "--global", "fmtprov"]),
        "n=3\nval=formatted\n"
    );
}

/// Several keys keep one line each under text, a miss an empty one.
#[test]
fn a_miss_holds_its_line_among_several_keys() {
    let d = TestDaemon::spawn();
    seed(&d);
    assert_eq!(
        ok(
            &d,
            &["get", "--global", "fmtprov.val", "nope.z", "fmtprov.n"]
        ),
        "formatted\n\n3\n"
    );
}

/// `status` names its delimited formats as `get` does: lowercase without a
/// header, uppercase with one, and a value as its raw text.
#[test]
fn status_csv_has_no_header_and_csv_upper_has_one() {
    let d = TestDaemon::spawn();
    seed(&d);
    let filter = ["--filter", "provider=fmtprov", "--sort", "field"];
    let plain = ok(&d, &[&["status", "-f", "csv"][..], &filter].concat());
    assert!(plain.starts_with("fmtprov,"), "no header: {plain}");
    assert!(plain.contains(",formatted,"), "raw value: {plain}");
    let headed = ok(&d, &[&["s.C"][..], &filter].concat());
    assert!(headed.starts_with("PROVIDER,PATH,FIELD"), "{headed}");
    let tsv = ok(&d, &[&["s.T"][..], &filter].concat());
    assert!(tsv.starts_with("PROVIDER\tPATH\tFIELD"), "{tsv}");
}

// ── one way to show a path under home ────────────────────────────────────────

/// The global entry is `~` in both tables, a path under home is written from
/// `~` in both, and `--filter path=~` selects the global entry.
#[test]
fn the_global_entry_and_paths_under_home_are_written_from_tilde() {
    let d = TestDaemon::spawn();
    seed(&d);
    let project = d.home.join("ws").join("project");
    std::fs::create_dir_all(&project).unwrap();
    ok(
        &d,
        &[
            "put",
            "--path",
            project.to_str().unwrap(),
            "fmtprov.at",
            "1",
        ],
    );
    for preset in ["human", "table"] {
        let out = ok(
            &d,
            &[
                "status",
                "-f",
                preset,
                "--color",
                "never",
                "--filter",
                "provider=fmtprov",
            ],
        );
        let rows: Vec<Vec<&str>> = out
            .lines()
            .skip(1)
            .map(|l| l.split_whitespace().collect())
            .collect();
        assert!(
            rows.iter()
                .any(|r| r.get(1) == Some(&"val") && r.contains(&"~")),
            "{preset}: the global entry is ~:\n{out}"
        );
        assert!(
            rows.iter()
                .any(|r| r.get(1) == Some(&"at") && r.contains(&"~/ws/project")),
            "{preset}: under home from ~:\n{out}"
        );
    }
    let global = ok(
        &d,
        &[
            "status",
            "-f",
            "{{ field }}",
            "--filter",
            "provider=fmtprov",
            "--filter",
            "path=~",
            "--sort",
            "field",
        ],
    );
    assert_eq!(global, "n\nval\n");
    refused(&d, &["status", "--filter", "path=-"], "path=~");
}

// ── a -f template sees what any template sees ────────────────────────────────

/// `args` with `FMT_X=x` in the environment, which must succeed.
fn ok_with_x(d: &TestDaemon, args: &[&str]) -> String {
    let out = d
        .comb(Path::new("/"))
        .env("FMT_X", "x")
        .args(args)
        .assert()
        .success();
    String::from_utf8(out.get_output().stdout.clone()).unwrap()
}

/// The value as `value`, an object's fields by name, and — as in
/// `comb eval` — `env`, provider refs and `comb.*`.
#[test]
fn a_get_template_sees_the_value_env_and_refs() {
    let d = TestDaemon::spawn();
    seed(&d);
    assert_eq!(
        ok_with_x(
            &d,
            &[
                "get",
                "--global",
                "-f",
                "{{ value }}|{{ env.FMT_X }}|{{ fmtprov.n }}|{{ comb.fmtprov.n }}",
                "fmtprov.val"
            ]
        ),
        "formatted|x|3|3"
    );
    assert_eq!(
        ok_with_x(&d, &["g.f", "{{ val }}/{{ n }}", "fmtprov", "--global"]),
        "formatted/3"
    );
}

#[test]
fn a_status_template_sees_the_row_and_env() {
    let d = TestDaemon::spawn();
    seed(&d);
    assert_eq!(
        ok_with_x(
            &d,
            &[
                "s.f",
                "{{ env.FMT_X }}:{{ field }}={{ value }}",
                "--filter",
                "provider=fmtprov",
                "--sort",
                "field"
            ]
        ),
        "x:n=3\nx:val=formatted\n"
    );
}

/// A template that does not compile fails the command once, named, with no
/// engine frame in the message.
#[test]
fn a_template_that_does_not_compile_fails_the_command() {
    let d = TestDaemon::spawn();
    seed(&d);
    for args in [
        &["get", "--global", "-f", "{{ x ", "fmtprov"][..],
        &["status", "-f", "{{ x ", "--filter", "provider=fmtprov"][..],
    ] {
        let out = d.comb(Path::new("/")).args(args).assert().code(2);
        let stderr = String::from_utf8_lossy(&out.get_output().stderr).to_string();
        assert!(stderr.contains("-f: template compile error:"), "{stderr}");
        assert!(!stderr.contains("(in <"), "no engine frame: {stderr}");
    }
}
