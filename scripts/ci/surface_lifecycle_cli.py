"""Recognize the bounded Clap constructor without weakening surface discovery."""

from __future__ import annotations

import re


COMMAND_MACRO = """
macro_rules! define_commands {
    ($(
        $(#[$group_attr:meta])*
        $variant:ident : $group:ident { $($variants:tt)* }
    )*) => {
        #[derive(Subcommand)]
        pub(super) enum Commands { $($($variants)*)* }
        $(
            $(#[$group_attr])*
            #[derive(Subcommand)]
            enum $group { $($variants)* }
        )*
        #[derive(Subcommand)]
        enum CommandGroups {
            $(
                $(#[$group_attr])*
                #[command(flatten)]
                $variant(Box<$group>),
            )*
        }
    };
}
"""
CLI_ARGS = """
#[derive(clap::Args)]
pub(super) struct Cli {
    #[command(subcommand)]
    pub(super) command: Commands,
}
"""
CLI_FACTORY = """
impl clap::CommandFactory for Cli {
    fn command() -> clap::Command {
        <CliBuilder as clap::CommandFactory>::command()
    }
    fn command_for_update() -> clap::Command {
        <CliBuilder as clap::CommandFactory>::command_for_update()
    }
}
"""


def _compact(text: str) -> str:
    # Whitespace inside a feature name is semantic, even when whitespace
    # between Rust tokens is not. Keep quoted literals byte-for-byte.
    return re.sub(r'"(?:\\.|[^"\\])*"|\s+',
                  lambda match: match[0] if match[0].startswith('"') else "", text)


def _reject_doc_comments(text: str) -> None:
    # Unlike ordinary comments, Rust doc comments become attributes consumed
    # by Clap. A doc on a flattened group can replace the root about text.
    if re.search(r"//(?:/(?!/)|!)|/\*(?:\*[^*]|!)", text):
        raise RuntimeError("bounded Clap constructor scaffolding cannot carry doc comments")


def _one(source: str, pattern: str, label: str) -> re.Match[str]:
    matches = list(re.finditer(pattern, source, re.S | re.M))
    if len(matches) != 1:
        raise RuntimeError(f"bounded Clap discovery requires exactly one {label}")
    return matches[0]


def _end(source, opening, left, right, matching):
    closing = matching(source, opening, left, right)
    if closing is None:
        raise RuntimeError(f"unclosed bounded Clap {left}{right}")
    return closing


def _space(source, cursor):
    while cursor < len(source) and source[cursor].isspace():
        cursor += 1
    return cursor


def _attributes(source, cursor, matching):
    attributes = []
    cursor = _space(source, cursor)
    while cursor < len(source) and source[cursor] == "#":
        opening = _space(source, cursor + 1)
        if source[opening:opening + 1] != "[":
            raise RuntimeError("unsupported bounded Clap group attribute")
        closing = _end(source, opening, "[", "]", matching)
        attributes.append(source[opening + 1:closing].strip())
        cursor = _space(source, closing + 1)
    return attributes, cursor


def _check_group_cfg(body, cfgs, matching, mask_comments):
    """A group cfg may be erased only when every variant already requires it."""
    source = mask_comments(body)
    cursor = 0
    count = 0
    while _space(source, cursor) < len(source):
        attributes, cursor = _attributes(source, cursor, matching)
        if not set(map(_compact, cfgs)).issubset(map(_compact, attributes)):
            raise RuntimeError("bounded Clap group cfg is not repeated on every variant")
        variant = re.match(r"[A-Za-z_][A-Za-z0-9_]*", source[cursor:])
        if not variant:
            raise RuntimeError("unsupported bounded Clap variant declaration")
        count += 1
        cursor = _space(source, cursor + variant.end())
        if source[cursor:cursor + 1] in ("{", "("):
            left = source[cursor]
            cursor = _space(source, _end(source, cursor, left, "}" if left == "{" else ")", matching) + 1)
        if cursor < len(source):
            if source[cursor] != ",":
                raise RuntimeError("unsupported bounded Clap variant separator")
            cursor += 1
    if not count:
        raise RuntimeError("bounded Clap groups must contain explicit variants")


def _expand_groups(text, matching, mask_comments):
    source = mask_comments(text)
    definition = _one(source, r"^\s*macro_rules!\s*define_commands\s*\{", "command macro")
    definition_end = _end(source, definition.end() - 1, "{", "}", matching)
    _reject_doc_comments(text[definition.start():definition_end + 1])
    if _compact(source[definition.start():definition_end + 1]) != _compact(COMMAND_MACRO):
        raise RuntimeError("unsupported bounded Clap command macro; review its expansion")
    invocation = _one(source, r"^\s*define_commands!\s*\{", "command macro invocation")
    invocation_end = _end(source, invocation.end() - 1, "{", "}", matching)
    if not definition_end < invocation.start():
        raise RuntimeError("bounded Clap command macro must precede its invocation")
    cursor = invocation.end()
    variants, labels, types = [], set(), set()
    while _space(source, cursor) < invocation_end:
        header_start = cursor
        attributes, cursor = _attributes(source, cursor, matching)
        if any(not re.fullmatch(r"cfg\s*\(.*\)", attr, re.S) for attr in attributes):
            raise RuntimeError("bounded Clap groups support only redundant cfg attributes")
        group = re.match(r"([A-Za-z_][A-Za-z0-9_]*)\s*:\s*([A-Za-z_][A-Za-z0-9_]*)\s*\{", source[cursor:])
        if not group or group[1] in labels or group[2] in types:
            raise RuntimeError("unsupported or duplicate bounded Clap group declaration")
        labels.add(group[1])
        types.add(group[2])
        opening = cursor + group.end() - 1
        _reject_doc_comments(text[header_start:opening])
        closing = _end(source, opening, "{", "}", matching)
        if closing >= invocation_end:
            raise RuntimeError("bounded Clap group escapes its invocation")
        body = text[opening + 1:closing]
        _check_group_cfg(body, attributes, matching, mask_comments)
        variants.append(body)
        cursor = closing + 1
    if not variants:
        raise RuntimeError("bounded Clap command invocation is empty")
    expanded = "#[derive(Subcommand)] pub(super) enum Commands {\n" + "\n".join(variants) + "\n}"
    return (text[:definition.start()] + text[definition_end + 1:invocation.start()]
            + expanded + text[invocation_end + 1:])


def _parser_declaration(source, name, derive, matching):
    pattern = (r"#\s*\[\s*derive\s*\([^]]*\b" + derive + r"\b[^]]*\)\s*\]"
               r"(?:\s*#\s*\[[^]]*\])*\s*(?:pub(?:\s*\([^)]*\))?\s+)?struct\s+"
               + name + r"\s*\{")
    match = _one(source, pattern, f"{name} declaration")
    return match, _end(source, match.end() - 1, "{", "}", matching)


def _normalize_factory(text, matching, mask_comments):
    source = mask_comments(text)
    cli, cli_end = _parser_declaration(source, "Cli", "Args", matching)
    if _compact(source[cli.start():cli_end + 1]) != _compact(CLI_ARGS):
        raise RuntimeError("unsupported bounded Clap Cli argument declaration")
    parser = _one(source, r"impl\s+clap::Parser\s+for\s+Cli\s*\{", "Cli Parser implementation")
    parser_end = _end(source, parser.end() - 1, "{", "}", matching)
    if source[parser.end():parser_end].strip():
        raise RuntimeError("bounded Clap Cli overrides Parser behavior")
    factory = _one(source, r"impl\s+clap::CommandFactory\s+for\s+Cli\s*\{", "Cli factory")
    factory_end = _end(source, factory.end() - 1, "{", "}", matching)
    if _compact(source[factory.start():factory_end + 1]) != _compact(CLI_FACTORY):
        raise RuntimeError("unsupported bounded Clap factory delegation")
    builder, builder_end = _parser_declaration(source, "CliBuilder", "Parser", matching)
    body = source[builder.end():builder_end]
    if _compact(body) != "#[command(subcommand)]command:CommandGroups,":
        raise RuntimeError("bounded Clap builder must expose exactly CommandGroups")
    header = text[builder.start():builder.end() - 1]
    explicit_group = re.compile(r"#\s*\[\s*group\s*\(\s*id\s*=\s*\"Cli\"\s*\)\s*\]\s*")
    if len(explicit_group.findall(header)) != 1:
        raise RuntimeError("bounded Clap builder must preserve the original Cli group")
    header = explicit_group.sub("", header)
    header = re.sub(r"struct\s+CliBuilder\s*$", "pub(super) struct Cli ", header)
    canonical = header + "{ #[command(subcommand)] pub(super) command: Commands, }"
    spans = [(cli.start(), cli_end + 1, canonical), (parser.start(), parser_end + 1, ""),
             (factory.start(), factory_end + 1, ""), (builder.start(), builder_end + 1, "")]
    for start, end, replacement in sorted(spans, reverse=True):
        text = text[:start] + replacement + text[end:]
    return text


def normalize_cli_construction(text, matching, mask_comments):
    """Return the equivalent flat declarations, or reject an unknown construction."""
    source = mask_comments(text)
    if not re.search(r"\b(?:macro_rules!\s*define_commands|define_commands!)", source):
        return text
    expanded = _expand_groups(text, matching, mask_comments)
    return _normalize_factory(expanded, matching, mask_comments)
