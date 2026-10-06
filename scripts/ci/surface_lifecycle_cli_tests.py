"""Offline regression coverage for bounded Clap surface discovery."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from surface_lifecycle_cli import CLI_ARGS, CLI_FACTORY, COMMAND_MACRO


ROOT_ATTRIBUTES = '#[derive(Parser)] #[command(name = "remem", about = "fixture", version)]'
CONTEXT = '#[command(visible_alias = "ctx")] Context { #[arg(long)] cwd: Option<String> },'
EVAL = '#[cfg(feature = "eval")] Eval,'
ADMIN = ('Admin { #[command(subcommand)] action: AdminAction }, '
         '#[cfg(feature = "off")] Hidden,')
NESTED = '#[derive(Subcommand)] enum AdminAction { #[command(alias = "save")] Backup }'
FLAT = (ROOT_ATTRIBUTES + ' pub(super) struct Cli { #[command(subcommand)] '
        'pub(super) command: Commands, } #[derive(Subcommand)] pub(super) enum Commands { '
        + CONTEXT + EVAL + ADMIN + '} ' + NESTED)
GROUPED = (CLI_ARGS + '\nimpl clap::Parser for Cli {}\n' + CLI_FACTORY + '\n'
           + ROOT_ATTRIBUTES + ' #[group(id = "Cli")] struct CliBuilder { '
           '#[command(subcommand)] command: CommandGroups, }\n' + COMMAND_MACRO
           + '\ndefine_commands! {\n'
           '    Runtime: RuntimeCommands { ' + CONTEXT + ' }\n'
           '    #[cfg(feature = "eval")]\n'
           '    Evaluation: EvaluationCommands { ' + EVAL + ' }\n'
           '    Maintenance: MaintenanceCommands { ' + ADMIN + ' }\n'
           '}\n' + NESTED)


class GroupedCliDiscoveryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="remem-grouped-cli-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "src/cli").mkdir(parents=True)
        self.source = self.root / "src/cli/types.rs"
        self.cargo = self.root / "Cargo.toml"
        self.cargo.write_text('[features]\ndefault = ["eval"]\neval = []\noff = []\n')

    def contracts(self, source):
        from surface_lifecycle_discovery import discover_cli_contracts
        self.source.write_text(source)
        return discover_cli_contracts(self.root)

    def test_flat_and_grouped_contracts_are_identical(self):
        expected = self.contracts(FLAT)
        self.assertEqual(self.contracts(GROUPED), expected)
        names = {item.split("@sha256=", 1)[0] for item in expected}
        self.assertEqual(names, {"remem context", "remem ctx", "remem eval", "remem admin",
                                 "remem admin backup", "remem admin save", "remem help",
                                 "remem admin help"})
        from surface_lifecycle_discovery import _subcommand_enums, discover_cli_commands
        self.assertEqual(discover_cli_commands(self.root), names)
        enums = _subcommand_enums(self.root, {"eval"})
        self.assertEqual([item[0][0] for item in enums["Commands"]], ["context", "eval", "admin"])

    def test_group_cfg_keeps_feature_disabled_commands_absent(self):
        self.cargo.write_text('[features]\ndefault = []\neval = []\noff = []\n')
        expected = self.contracts(FLAT)
        self.assertEqual(self.contracts(GROUPED), expected)
        self.assertFalse(any(item.startswith("remem eval@") for item in expected))

    def test_fields_aliases_and_root_metadata_still_change_fingerprints(self):
        expected = self.contracts(GROUPED)
        for old, new in [("cwd: Option<String>", "project: Option<String>"),
                         ('visible_alias = "ctx"', 'visible_alias = "start"'),
                         ('about = "fixture"', 'about = "updated fixture"'),
                         ('name = "remem"', 'name = "memory"'),
                         ('#[cfg(feature = "off")] Hidden', 'Hidden')]:
            with self.subTest(mutation=new):
                self.assertNotEqual(self.contracts(GROUPED.replace(old, new)), expected)

    def test_unknown_macro_or_factory_semantics_fail_closed(self):
        mutations = [
            GROUPED.replace('$variant(Box<$group>)', '$variant($group)'),
            GROUPED.replace('#[command(flatten)]', '#[command(subcommand)]'),
            GROUPED.replace('>::command_for_update()', '>::command()'),
            GROUPED.replace('impl clap::Parser for Cli {}',
                            'impl clap::Parser for Cli { fn parse() -> Self { todo!() } }'),
            GROUPED.replace('command: CommandGroups,', 'command: Commands,'),
            GROUPED.replace('#[group(id = "Cli")]', '#[group(id = "CliBuilder")]'),
            GROUPED.replace('pub(super) command: Commands,', 'pub(super) command: Commands, extra: bool,'),
            GROUPED.replace('Runtime: RuntimeCommands {', 'Runtime(RuntimeCommands) {'),
            GROUPED.replace('Maintenance: MaintenanceCommands {', 'Runtime: RuntimeCommands {'),
            GROUPED.replace('    #[cfg(feature = "eval")]\n    Evaluation:',
                            '    #[cfg(feature = "off")]\n    Evaluation:'),
            GROUPED.replace('    #[cfg(feature = "eval")]\n    Evaluation:',
                            '    #[cfg(feature = "e val")]\n    Evaluation:'),
            GROUPED.replace('    Runtime: RuntimeCommands {',
                            '    #[command(about = "replacement")]\n    Runtime: RuntimeCommands {'),
            GROUPED + '\ndefine_commands! {}\n',
            GROUPED + '\n' + COMMAND_MACRO,
        ]
        for index, source in enumerate(mutations):
            with self.subTest(mutation=index), self.assertRaises(RuntimeError):
                self.contracts(source)

    def test_group_doc_comments_are_attributes_not_ignorable_comments(self):
        mutations = [
            GROUPED.replace('            enum $group {',
                            '            /// Replace the root about\n            enum $group {'),
            GROUPED.replace('#[derive(Subcommand)]\n            enum $group',
                            '#[derive(Subcommand)] /// Replace the root about\n            enum $group'),
            GROUPED.replace('    Runtime: RuntimeCommands {',
                            '    /// Replace the root about\n    Runtime: RuntimeCommands {'),
            GROUPED.replace('    Runtime: RuntimeCommands {',
                            '    /** Replace the root about */\n    Runtime: RuntimeCommands {'),
        ]
        for index, source in enumerate(mutations):
            with self.subTest(mutation=index), self.assertRaises(RuntimeError):
                self.contracts(source)

    def test_pending_enum_file_move_preserves_its_contract(self):
        expected = self.contracts(GROUPED)
        pending = self.root / "src/cli/pending_types.rs"
        pending.write_text(NESTED)
        self.assertEqual(self.contracts(GROUPED.removesuffix(NESTED)), expected)


def grouped_cli_self_test() -> int:
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(GroupedCliDiscoveryTests)
    result = unittest.TextTestRunner(verbosity=1).run(suite)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    raise SystemExit(grouped_cli_self_test())
