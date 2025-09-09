use indoc::indoc;
use serde::Deserialize;
use serde::Serialize;

use mago_fixer::SafetyClassification;
use mago_php_version::PHPVersion;
use mago_php_version::PHPVersionRange;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_reporting::Level;
use mago_span::HasSpan;
use mago_syntax::ast::Argument;
use mago_syntax::ast::Node;
use mago_syntax::ast::NodeKind;

use crate::category::Category;
use crate::context::LintContext;
use crate::requirements::RuleRequirements;
use crate::rule::Config;
use crate::rule::LintRule;
use crate::rule_meta::RuleMeta;
use crate::settings::RuleSettings;

#[derive(Debug, Clone)]
pub struct NamedArgumentOrderingRule {
    meta: &'static RuleMeta,
    cfg: NamedArgumentOrderingConfig,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct NamedArgumentOrderingConfig {
    pub level: Level,
}

impl Default for NamedArgumentOrderingConfig {
    fn default() -> Self {
        Self { level: Level::Warning }
    }
}

impl Config for NamedArgumentOrderingConfig {
    fn level(&self) -> Level {
        self.level
    }
}

impl LintRule for NamedArgumentOrderingRule {
    type Config = NamedArgumentOrderingConfig;

    fn meta() -> &'static RuleMeta {
        const META: RuleMeta = RuleMeta {
            name: "Named Argument Ordering",
            code: "named-argument-ordering",
            description: indoc! {r#"
                Enforces that named arguments are ordered alphabetically by parameter name.

                This improves code readability and consistency by ensuring that named arguments
                follow a predictable order, making it easier to scan and understand function calls
                with multiple named parameters.
            "#},
            good_example: indoc! {r#"
                <?php

                function configure(string $host, int $port, bool $ssl, string $username) {}

                configure(host: 'localhost', port: 8080, ssl: true, username: 'admin'); // ✅ alphabetical order
            "#},
            bad_example: indoc! {r#"
                <?php

                function configure(string $host, int $port, bool $ssl, string $username) {}

                configure(username: 'admin', host: 'localhost', ssl: true, port: 8080); // ❌ not alphabetical
            "#},
            category: Category::Consistency,
            requirements: RuleRequirements::PHPVersion(PHPVersionRange::from(PHPVersion::PHP80)),
        };
        &META
    }

    fn targets() -> &'static [NodeKind] {
        const TARGETS: &[NodeKind] = &[NodeKind::ArgumentList];

        TARGETS
    }

    fn build(settings: RuleSettings<Self::Config>) -> Self {
        Self { meta: Self::meta(), cfg: settings.config }
    }

    fn check<'ast, 'arena>(&self, ctx: &mut LintContext<'_, 'arena>, node: Node<'ast, 'arena>) {
        let Node::ArgumentList(argument_list) = node else {
            return;
        };

        // Extract named arguments and their positions
        let mut named_args: Vec<(usize, &str, &Argument<'arena>)> = Vec::new();
        let mut has_positional_after_named = false;
        let mut found_named = false;

        for (index, argument) in argument_list.arguments.as_slice().iter().enumerate() {
            match argument {
                Argument::Named(named_arg) => {
                    found_named = true;
                    let name_span = named_arg.name.span();
                    let name =
                        &ctx.source_file.contents[name_span.start.offset as usize..name_span.end.offset as usize];
                    named_args.push((index, name, argument));
                }
                Argument::Positional(_) => {
                    if found_named {
                        has_positional_after_named = true;
                    }
                }
            }
        }

        // Skip if there are fewer than 2 named arguments or if there are positional args after named ones
        if named_args.len() < 2 || has_positional_after_named {
            return;
        }

        // Check if named arguments are already sorted
        let mut sorted_names: Vec<&str> = named_args.iter().map(|(_, name, _)| *name).collect();
        sorted_names.sort();

        let current_names: Vec<&str> = named_args.iter().map(|(_, name, _)| *name).collect();

        if current_names == sorted_names {
            return; // Already sorted
        }

        // Create the issue with auto-fix
        let issue = Issue::new(self.cfg.level, "Named arguments should be ordered alphabetically by parameter name.")
            .with_code(self.meta.code)
            .with_annotation(
                Annotation::primary(argument_list.span())
                    .with_message("These named arguments are not in alphabetical order."),
            )
            .with_help("Consider reordering the named arguments alphabetically to improve readability.");

        ctx.collector.propose(issue, |plan| {
            // Create a mapping from current order to sorted order
            let mut sorted_args = named_args.clone();
            sorted_args.sort_by_key(|(_, name, _)| *name);

            // Build the replacement text for the entire argument list
            let mut replacement_parts = Vec::new();
            let mut current_arg_index = 0;

            for argument in argument_list.arguments.as_slice() {
                match argument {
                    Argument::Named(_) => {
                        // Find this named argument in the sorted list
                        let (_, _sorted_name, sorted_arg) = &sorted_args[current_arg_index];

                        // Get the text for the sorted argument
                        let arg_text = match sorted_arg {
                            Argument::Named(named_arg) => {
                                let name_span = named_arg.name.span();
                                let name_text = &ctx.source_file.contents
                                    [name_span.start.offset as usize..name_span.end.offset as usize];
                                let value_span = named_arg.value.span();
                                let value_text = &ctx.source_file.contents
                                    [value_span.start.offset as usize..value_span.end.offset as usize];
                                format!("{}: {}", name_text, value_text)
                            }
                            _ => unreachable!("We only collected named arguments"),
                        };

                        replacement_parts.push(arg_text);
                        current_arg_index += 1;
                    }
                    Argument::Positional(_) => {
                        // Keep positional arguments as-is
                        let arg_span = argument.span();
                        let arg_text =
                            &ctx.source_file.contents[arg_span.start.offset as usize..arg_span.end.offset as usize];
                        replacement_parts.push(arg_text.to_string());
                    }
                }
            }

            // Join the arguments with commas and spaces
            let replacement_text = replacement_parts.join(", ");

            // Replace the entire arguments section (between parentheses)
            let args_start = argument_list.left_parenthesis.end.offset;
            let args_end = argument_list.right_parenthesis.start.offset;

            plan.replace(args_start..args_end, replacement_text, SafetyClassification::Safe);
        });
    }
}
