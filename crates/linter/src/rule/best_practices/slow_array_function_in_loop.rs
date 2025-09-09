use indoc::indoc;
use serde::Deserialize;
use serde::Serialize;

use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_reporting::Level;
use mago_span::HasSpan;
use mago_syntax::ast::*;

use crate::category::Category;
use crate::context::LintContext;
use crate::requirements::RuleRequirements;
use crate::rule::Config;
use crate::rule::LintRule;
use crate::rule::utils::call::function_call_matches_any;
use crate::rule_meta::RuleMeta;
use crate::settings::RuleSettings;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct SlowArrayFunctionInLoopConfig {
    pub level: Level,
    pub detect_array_merge: bool,
    pub detect_count_functions: bool,
    pub detect_in_nested_loops: bool,
    pub auto_fix_array_merge: bool,
    pub auto_fix_count_in_loop: bool,
    pub min_loop_iterations_threshold: usize,
    pub require_safe_count_caching: bool,
    pub max_auto_fix_complexity: usize,
}

impl Default for SlowArrayFunctionInLoopConfig {
    fn default() -> Self {
        Self {
            level: Level::Warning,
            detect_array_merge: true,
            detect_count_functions: true,
            detect_in_nested_loops: true,
            auto_fix_array_merge: true,
            auto_fix_count_in_loop: true,
            min_loop_iterations_threshold: 2,
            require_safe_count_caching: true,
            max_auto_fix_complexity: 10,
        }
    }
}

impl Config for SlowArrayFunctionInLoopConfig {
    fn level(&self) -> Level {
        self.level
    }
}

#[derive(Debug, Clone)]
pub struct SlowArrayFunctionInLoopRule {
    meta: &'static RuleMeta,
    cfg: SlowArrayFunctionInLoopConfig,
}

impl LintRule for SlowArrayFunctionInLoopRule {
    type Config = SlowArrayFunctionInLoopConfig;

    fn meta() -> &'static RuleMeta {
        const META: RuleMeta = RuleMeta {
            name: "Slow Array Function in Loop",
            code: "slow-array-function-in-loop",
            description: indoc! {r#"
                Detects performance anti-patterns where slow array functions are used inside loops,
                causing unnecessary computational overhead and high CPU usage.

                This rule identifies two main patterns:
                1. Array merging in loops - `array_merge()` called repeatedly in loop iterations
                2. Count function in loop conditions - `count()`, `sizeof()` called in loop conditions

                Array merging in loops has O(n²) complexity and can be 10-100x slower than collecting
                arrays and merging once. Count functions in loop conditions cause unnecessary overhead
                when the array doesn't change.
            "#},
            good_example: indoc! {r#"
                <?php

                // Collect arrays, then merge once
                $options = [];
                foreach ($sources as $source) {
                    $options[] = $source->getOptions();
                }
                $options = array_merge(...$options);

                // Cache count result
                for ($i = 0, $count = count($array); $i < $count; $i++) {
                    echo $array[$i];
                }
            "#},
            bad_example: indoc! {r#"
                <?php

                // Slow array merging in loop
                $options = [];
                foreach ($sources as $source) {
                    $options = array_merge($options, $source->getOptions());
                }

                // Count called on every iteration
                for ($i = 0; $i < count($array); $i++) {
                    echo $array[$i];
                }
            "#},
            category: Category::BestPractices,
            requirements: RuleRequirements::None,
        };

        &META
    }

    fn targets() -> &'static [NodeKind] {
        const TARGETS: &[NodeKind] =
            &[NodeKind::For, NodeKind::Foreach, NodeKind::While, NodeKind::DoWhile, NodeKind::FunctionCall];

        TARGETS
    }

    fn build(settings: RuleSettings<Self::Config>) -> Self {
        Self { meta: Self::meta(), cfg: settings.config }
    }

    fn check<'ast, 'arena>(&self, ctx: &mut LintContext<'_, 'arena>, node: Node<'ast, 'arena>) {
        match node {
            Node::For(for_loop) => {
                if self.cfg.detect_count_functions {
                    // For loops can have multiple conditions, check all of them
                    for condition in for_loop.conditions.iter() {
                        if let Some(count_call) = Self::find_count_function_call(ctx, condition) {
                            self.report_count_in_condition(ctx, count_call, "for loop");
                        }
                    }
                }
                if self.cfg.detect_array_merge {
                    self.check_array_merge_in_for_body(ctx, for_loop);
                }
            }
            Node::Foreach(foreach) => {
                if self.cfg.detect_array_merge {
                    self.check_array_merge_in_foreach_body(ctx, foreach);
                }
            }
            Node::While(while_loop) => {
                if self.cfg.detect_count_functions
                    && let Some(count_call) = Self::find_count_function_call(ctx, while_loop.condition)
                {
                    self.report_count_in_condition(ctx, count_call, "while loop");
                }
                if self.cfg.detect_array_merge {
                    self.check_array_merge_in_while_body(ctx, while_loop);
                }
            }
            Node::DoWhile(do_while) => {
                if self.cfg.detect_count_functions
                    && let Some(count_call) = Self::find_count_function_call(ctx, do_while.condition)
                {
                    self.report_count_in_condition(ctx, count_call, "do-while loop");
                }
                if self.cfg.detect_array_merge {
                    self.check_array_merge_in_do_while_body(ctx, do_while);
                }
            }
            Node::FunctionCall(_function_call) => {
                // This handles function calls that might be inside loops
                // For now, we rely on the loop-specific checks above
            }
            _ => {}
        }
    }
}

impl SlowArrayFunctionInLoopRule {
    fn find_count_function_call<'a>(
        ctx: &LintContext<'_, '_>,
        expression: &'a Expression<'a>,
    ) -> Option<&'a FunctionCall<'a>> {
        match expression {
            Expression::Call(Call::Function(call)) => {
                if function_call_matches_any(ctx, call, &["count", "sizeof"]).is_some() { Some(call) } else { None }
            }
            Expression::Binary(binary) => {
                // Check both sides of binary expressions (e.g., $i < count($array))
                Self::find_count_function_call(ctx, binary.lhs)
                    .or_else(|| Self::find_count_function_call(ctx, binary.rhs))
            }
            Expression::Parenthesized(paren) => Self::find_count_function_call(ctx, paren.expression),
            _ => None,
        }
    }

    fn report_count_in_condition(&self, ctx: &mut LintContext<'_, '_>, count_call: &FunctionCall<'_>, loop_type: &str) {
        let issue = Issue::new(self.cfg.level, format!("Array count function called in {} condition.", loop_type))
            .with_code(self.meta.code)
            .with_annotation(
                Annotation::primary(count_call.span()).with_message("This count() call executes on every iteration"),
            )
            .with_note("Calling count() in loop conditions causes unnecessary function call overhead.")
            .with_help("Cache the count result: for ($i = 0, $count = count($array); $i < $count; $i++)");

        // Note: Auto-fix for count caching is complex and would require significant
        // AST manipulation to properly insert the count variable in the for loop initialization.
        // For now, we provide the warning and help text. Auto-fix could be implemented
        // in a future version with more sophisticated AST transformation capabilities.

        ctx.collector.report(issue);
    }

    fn check_array_merge_in_for_body(&self, ctx: &mut LintContext<'_, '_>, for_loop: &For<'_>) {
        match &for_loop.body {
            ForBody::Statement(stmt) => {
                self.check_array_merge_in_statement(ctx, stmt);
            }
            ForBody::ColonDelimited(body) => {
                for stmt in body.statements.iter() {
                    self.check_array_merge_in_statement(ctx, stmt);
                }
            }
        }
    }

    fn check_array_merge_in_foreach_body(&self, ctx: &mut LintContext<'_, '_>, foreach: &Foreach<'_>) {
        match &foreach.body {
            ForeachBody::Statement(stmt) => {
                self.check_array_merge_in_statement(ctx, stmt);
            }
            ForeachBody::ColonDelimited(body) => {
                for stmt in body.statements.iter() {
                    self.check_array_merge_in_statement(ctx, stmt);
                }
            }
        }
    }

    fn check_array_merge_in_while_body(&self, ctx: &mut LintContext<'_, '_>, while_loop: &While<'_>) {
        match &while_loop.body {
            WhileBody::Statement(stmt) => {
                self.check_array_merge_in_statement(ctx, stmt);
            }
            WhileBody::ColonDelimited(body) => {
                for stmt in body.statements.iter() {
                    self.check_array_merge_in_statement(ctx, stmt);
                }
            }
        }
    }

    fn check_array_merge_in_do_while_body(&self, ctx: &mut LintContext<'_, '_>, do_while: &DoWhile<'_>) {
        self.check_array_merge_in_statement(ctx, do_while.statement);
    }

    fn check_array_merge_in_statement(&self, ctx: &mut LintContext<'_, '_>, statement: &Statement<'_>) {
        match statement {
            Statement::Expression(expr_stmt) => {
                self.check_array_merge_in_expression(ctx, expr_stmt.expression);
            }
            Statement::Block(block) => {
                for stmt in block.statements.iter() {
                    self.check_array_merge_in_statement(ctx, stmt);
                }
            }
            _ => {}
        }
    }

    fn check_array_merge_in_expression(&self, ctx: &mut LintContext<'_, '_>, expression: &Expression<'_>) {
        if let Expression::Assignment(assignment) = expression
            && let Expression::Call(Call::Function(call)) = assignment.rhs
            && function_call_matches_any(ctx, call, &["array_merge"]).is_some()
            && self.is_accumulative_array_merge(ctx, assignment, call)
        {
            self.report_array_merge_in_loop(ctx, assignment, call);
        }
    }

    fn is_accumulative_array_merge(
        &self,
        ctx: &mut LintContext<'_, '_>,
        assignment: &Assignment<'_>,
        call: &FunctionCall<'_>,
    ) -> bool {
        // Check if the first argument of array_merge is the same variable being assigned to
        if let Some(Argument::Positional(pos_arg)) = call.argument_list.arguments.first() {
            let assignment_lhs_text = &ctx.source_file.contents[assignment.lhs.span().to_range_usize()];
            let first_arg_text = &ctx.source_file.contents[pos_arg.value.span().to_range_usize()];

            return assignment_lhs_text == first_arg_text;
        }
        false
    }

    fn report_array_merge_in_loop(
        &self,
        ctx: &mut LintContext<'_, '_>,
        assignment: &Assignment<'_>,
        _call: &FunctionCall<'_>,
    ) {
        let issue = Issue::new(
            self.cfg.level,
            "Slow array merging in loop detected.",
        )
        .with_code(self.meta.code)
        .with_annotation(
            Annotation::primary(assignment.span())
                .with_message("This array_merge call executes on every iteration"),
        )
        .with_note("Merging arrays in loops has O(n²) complexity and causes high CPU usage.")
        .with_help("Collect arrays in the loop, then merge once: $options[] = $source->getOptions(); array_merge(...$options);");

        // Note: Auto-fix for array_merge pattern is complex and potentially unsafe
        // as it changes execution order and may affect side effects. For now, we provide
        // the warning and help text. Auto-fix could be implemented in a future version
        // with careful safety analysis.

        ctx.collector.report(issue);
    }
}
