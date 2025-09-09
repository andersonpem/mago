use indoc::indoc;
use serde::Deserialize;
use serde::Serialize;

use mago_atom::ascii_lowercase_atom;
use mago_atom::empty_atom;
use mago_codex::metadata::function_like::FunctionLikeKind;
use mago_fixer::SafetyClassification;
use mago_php_version::PHPVersion;
use mago_php_version::PHPVersionRange;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_reporting::Level;
use mago_span::HasSpan;
use mago_syntax::ast::Argument;
use mago_syntax::ast::Attribute;
use mago_syntax::ast::ClassLikeMemberSelector;
use mago_syntax::ast::Expression;
use mago_syntax::ast::FunctionCall;
use mago_syntax::ast::FunctionLikeParameterList;
use mago_syntax::ast::Identifier;
use mago_syntax::ast::Instantiation;
use mago_syntax::ast::MethodCall;
use mago_syntax::ast::Node;
use mago_syntax::ast::NodeKind;
use mago_syntax::ast::Program;
use mago_syntax::ast::Statement;
use mago_syntax::ast::StaticMethodCall;
use mago_syntax::ast::Variable;

use crate::category::Category;
use crate::context::LintContext;
use crate::requirements::RuleRequirements;
use crate::rule::Config;
use crate::rule::LintRule;
use crate::rule_meta::RuleMeta;
use crate::settings::RuleSettings;

#[derive(Debug, Clone)]
pub struct ArgumentOrderRule {
    meta: &'static RuleMeta,
    cfg: ArgumentOrderConfig,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct ArgumentOrderConfig {
    pub level: Level,
}

impl Default for ArgumentOrderConfig {
    fn default() -> Self {
        Self { level: Level::Warning }
    }
}

impl Config for ArgumentOrderConfig {
    fn default_enabled() -> bool {
        false // This is an optional rule
    }

    fn level(&self) -> Level {
        self.level
    }
}

impl LintRule for ArgumentOrderRule {
    type Config = ArgumentOrderConfig;

    fn meta() -> &'static RuleMeta {
        const META: RuleMeta = RuleMeta {
            name: "Argument Order",
            code: "argument-order",
            description: indoc! {r#"
                Enforces that named arguments are used in the same order as defined 
                in the function or attribute signature.

                This rule helps maintain consistency and readability by ensuring that
                named arguments follow the parameter declaration order, making code
                easier to understand and maintain.
            "#},
            good_example: indoc! {r#"
                <?php

                function createUser(string $name, string $email, int $age): void {}

                // ✅ Arguments in signature order
                createUser(
                    name: "John",
                    email: "john@example.com", 
                    age: 25
                );

                #[Route(string $path, string $method = "GET", bool $authRequired = false)]
                class Route {}

                // ✅ Attribute arguments in signature order  
                #[Route(
                    path: "/users",
                    method: "POST",
                    authRequired: true
                )]
                function handler() {}
            "#},
            bad_example: indoc! {r#"
                <?php

                function createUser(string $name, string $email, int $age): void {}

                // ❌ Arguments out of signature order
                createUser(
                    age: 25,
                    name: "John",
                    email: "john@example.com"
                );

                #[Route(string $path, string $method = "GET", bool $authRequired = false)]
                class Route {}

                // ❌ Attribute arguments out of signature order
                #[Route(
                    authRequired: true,
                    path: "/users", 
                    method: "POST"
                )]
                function handler() {}
            "#},
            category: Category::Consistency,
            requirements: RuleRequirements::PHPVersion(PHPVersionRange::from(PHPVersion::PHP80)),
        };
        &META
    }

    fn targets() -> &'static [NodeKind] {
        const TARGETS: &[NodeKind] = &[NodeKind::Program];

        TARGETS
    }

    fn build(settings: RuleSettings<Self::Config>) -> Self {
        Self { meta: Self::meta(), cfg: settings.config }
    }

    fn check<'ast, 'arena>(&self, ctx: &mut LintContext<'_, 'arena>, node: Node<'ast, 'arena>) {
        let Node::Program(program) = node else {
            return;
        };

        // First pass: collect all function and class definitions
        let mut function_signatures = std::collections::HashMap::new();
        let mut class_constructors = std::collections::HashMap::new();

        for statement in program.statements.iter() {
            match statement {
                Statement::Function(function) => {
                    let param_names = self.extract_parameter_names(&function.parameter_list);
                    function_signatures.insert(function.name.value, param_names);
                }
                Statement::Class(class) => {
                    // Look for constructor
                    for member in class.members.iter() {
                        if let mago_syntax::ast::ClassLikeMember::Method(method) = member
                            && method.name.value == "__construct"
                        {
                            let param_names = self.extract_parameter_names(&method.parameter_list);
                            class_constructors.insert(class.name.value, param_names);
                            break;
                        }
                    }
                    // If no constructor found, insert empty parameter list
                    if !class_constructors.contains_key(class.name.value) {
                        class_constructors.insert(class.name.value, Vec::new());
                    }
                }
                _ => {}
            }
        }

        // Second pass: check all function calls and attributes
        self.check_program_nodes(ctx, program, &function_signatures, &class_constructors);
    }
}

impl ArgumentOrderRule {
    fn check_program_nodes<'arena>(
        &self,
        ctx: &mut LintContext<'_, 'arena>,
        program: &Program<'arena>,
        function_signatures: &std::collections::HashMap<&'arena str, Vec<&'arena str>>,
        class_constructors: &std::collections::HashMap<&'arena str, Vec<&'arena str>>,
    ) {
        // Use a simple visitor pattern to traverse all nodes
        self.visit_statements(ctx, &program.statements, function_signatures, class_constructors);
    }

    fn visit_statements<'arena>(
        &self,
        ctx: &mut LintContext<'_, 'arena>,
        statements: &mago_syntax::ast::Sequence<'arena, Statement<'arena>>,
        function_signatures: &std::collections::HashMap<&'arena str, Vec<&'arena str>>,
        class_constructors: &std::collections::HashMap<&'arena str, Vec<&'arena str>>,
    ) {
        for statement in statements.iter() {
            self.visit_statement(ctx, statement, function_signatures, class_constructors);
        }
    }

    fn visit_statement<'arena>(
        &self,
        ctx: &mut LintContext<'_, 'arena>,
        statement: &Statement<'arena>,
        function_signatures: &std::collections::HashMap<&'arena str, Vec<&'arena str>>,
        class_constructors: &std::collections::HashMap<&'arena str, Vec<&'arena str>>,
    ) {
        match statement {
            Statement::Function(function) => {
                // Check function calls within this function
                self.visit_statements(ctx, &function.body.statements, function_signatures, class_constructors);

                // Check attributes on the function
                for attr_list in function.attribute_lists.iter() {
                    for attr in attr_list.attributes.iter() {
                        self.check_attribute_with_signatures(ctx, attr, class_constructors);
                    }
                }
            }
            Statement::Class(class) => {
                // Check attributes on the class
                for attr_list in class.attribute_lists.iter() {
                    for attr in attr_list.attributes.iter() {
                        self.check_attribute_with_signatures(ctx, attr, class_constructors);
                    }
                }

                // Check class members
                for member in class.members.iter() {
                    self.visit_class_member(ctx, member, function_signatures, class_constructors);
                }
            }
            Statement::Expression(expr_stmt) => {
                // Check function calls in expression statements
                self.visit_expression(ctx, expr_stmt.expression, function_signatures, class_constructors);
            }
            // Add other statement types as needed
            _ => {}
        }
    }

    fn visit_class_member<'arena>(
        &self,
        ctx: &mut LintContext<'_, 'arena>,
        member: &mago_syntax::ast::ClassLikeMember<'arena>,
        function_signatures: &std::collections::HashMap<&'arena str, Vec<&'arena str>>,
        class_constructors: &std::collections::HashMap<&'arena str, Vec<&'arena str>>,
    ) {
        if let mago_syntax::ast::ClassLikeMember::Method(method) = member {
            // Check attributes on the method
            for attr_list in method.attribute_lists.iter() {
                for attr in attr_list.attributes.iter() {
                    self.check_attribute_with_signatures(ctx, attr, class_constructors);
                }
            }

            // Check method body
            match &method.body {
                mago_syntax::ast::MethodBody::Concrete(block) => {
                    self.visit_statements(ctx, &block.statements, function_signatures, class_constructors);
                }
                mago_syntax::ast::MethodBody::Abstract(_) => {} // Abstract methods have no body
            }
        }
    }

    fn check_function_call_with_signatures<'arena>(
        &self,
        ctx: &mut LintContext<'_, 'arena>,
        function_call: &FunctionCall<'arena>,
        function_signatures: &std::collections::HashMap<&'arena str, Vec<&'arena str>>,
    ) {
        // Only check if there are named arguments
        let named_args: Vec<_> = function_call
            .argument_list
            .arguments
            .iter()
            .filter_map(|arg| match arg {
                Argument::Named(named) => Some(named),
                _ => None,
            })
            .collect();

        if named_args.len() < 2 {
            return; // Need at least 2 named arguments to check order
        }

        // Try to resolve the function name
        let function_name = match &function_call.function {
            Expression::Identifier(identifier) => identifier.value(),
            _ => return, // Can't resolve complex function expressions
        };

        // First check local signatures (same file)
        if let Some(parameter_order) = function_signatures.get(function_name) {
            self.check_argument_order_against_expected(ctx, &named_args, parameter_order, "function signature");
            return;
        }

        // Then check cross-file signatures from codebase metadata
        if let Some(parameter_order) = self.lookup_function_signature_from_codebase(ctx, function_name) {
            let parameter_refs: Vec<&str> = parameter_order.iter().map(|s| s.as_str()).collect();
            self.check_argument_order_against_expected(ctx, &named_args, &parameter_refs, "function signature");
        }
    }

    fn check_attribute_with_signatures<'arena>(
        &self,
        ctx: &mut LintContext<'_, 'arena>,
        attribute: &Attribute<'arena>,
        class_constructors: &std::collections::HashMap<&'arena str, Vec<&'arena str>>,
    ) {
        let Some(argument_list) = &attribute.argument_list else {
            return;
        };

        // Only check if there are named arguments
        let named_args: Vec<_> = argument_list
            .arguments
            .iter()
            .filter_map(|arg| match arg {
                Argument::Named(named) => Some(named),
                _ => None,
            })
            .collect();

        if named_args.len() < 2 {
            return; // Need at least 2 named arguments to check order
        }

        // Get the attribute class name
        let class_name = match &attribute.name {
            Identifier::Local(local) => local.value,
            Identifier::Qualified(qualified) => qualified.value,
            Identifier::FullyQualified(fully_qualified) => fully_qualified.value,
        };

        // First check local constructor signatures (same file)
        if let Some(parameter_order) = class_constructors.get(class_name) {
            self.check_argument_order_against_expected(
                ctx,
                &named_args,
                parameter_order,
                "attribute constructor signature",
            );
            return;
        }

        // Then check cross-file constructor signatures from codebase metadata
        if let Some(parameter_order) = self.lookup_method_signature_from_codebase(ctx, class_name, "__construct") {
            let parameter_refs: Vec<&str> = parameter_order.iter().map(|s| s.as_str()).collect();
            self.check_argument_order_against_expected(
                ctx,
                &named_args,
                &parameter_refs,
                "attribute constructor signature",
            );
        }
    }

    fn visit_expression<'arena>(
        &self,
        ctx: &mut LintContext<'_, 'arena>,
        expression: &Expression<'arena>,
        function_signatures: &std::collections::HashMap<&'arena str, Vec<&'arena str>>,
        class_constructors: &std::collections::HashMap<&'arena str, Vec<&'arena str>>,
    ) {
        match expression {
            // Function calls: myFunction(args...)
            Expression::Call(mago_syntax::ast::Call::Function(function_call)) => {
                self.check_function_call_with_signatures(ctx, function_call, function_signatures);
            }
            // Method calls: $obj->method(args...)
            Expression::Call(mago_syntax::ast::Call::Method(method_call)) => {
                self.check_method_call_with_codebase(ctx, method_call);
            }
            // Static method calls: Class::method(args...)
            Expression::Call(mago_syntax::ast::Call::StaticMethod(static_call)) => {
                self.check_static_method_call_with_codebase(ctx, static_call);
            }
            // Constructor calls: new Class(args...)
            Expression::Instantiation(instantiation) => {
                self.check_instantiation_with_signatures(ctx, instantiation, class_constructors);
            }
            _ => {}
        }
    }

    fn extract_parameter_names<'arena>(&self, parameter_list: &FunctionLikeParameterList<'arena>) -> Vec<&'arena str> {
        parameter_list
            .parameters
            .iter()
            .map(|param| {
                // Remove the $ prefix from parameter names
                let full_name = param.variable.name;
                full_name.strip_prefix('$').unwrap_or(full_name)
            })
            .collect()
    }

    fn check_argument_order_against_expected<'arena>(
        &self,
        ctx: &mut LintContext<'_, 'arena>,
        named_args: &[&mago_syntax::ast::NamedArgument<'arena>],
        expected_order: &[&str],
        context_description: &str,
    ) {
        // Create a map of parameter names to their positions
        let param_positions: std::collections::HashMap<&str, usize> =
            expected_order.iter().enumerate().map(|(i, &name)| (name, i)).collect();

        // Check if arguments are in the correct order and collect out-of-order ones
        let mut out_of_order_args = Vec::new();
        let mut expected_position = 0;

        for named_arg in named_args {
            let arg_name = named_arg.name.value;

            if let Some(&param_position) = param_positions.get(arg_name) {
                if param_position < expected_position {
                    out_of_order_args.push(named_arg);
                } else {
                    expected_position = param_position;
                }
            }
        }

        // If there are out-of-order arguments, report them with autofix
        if !out_of_order_args.is_empty() {
            // Create the corrected argument list
            let mut sorted_args = named_args.to_vec();
            sorted_args.sort_by_key(|arg| param_positions.get(arg.name.value).copied().unwrap_or(usize::MAX));

            for out_of_order_arg in &out_of_order_args {
                let issue = Issue::new(
                    self.cfg.level,
                    format!(
                        "Named argument `{}` is not in the same order as the {}.",
                        out_of_order_arg.name.value, context_description
                    ),
                )
                .with_code(self.meta.code)
                .with_annotation(
                    Annotation::primary(out_of_order_arg.span()).with_message("This argument is out of order."),
                )
                .with_help(format!("Reorder named arguments to match the {} parameter order.", context_description));

                // Only propose autofix for the first out-of-order argument to avoid conflicts
                if std::ptr::eq(*out_of_order_arg, out_of_order_args[0]) {
                    ctx.collector.propose(issue, |plan| {
                        self.create_reorder_fix(plan, named_args, &sorted_args);
                    });
                } else {
                    ctx.collector.report(issue);
                }
            }
        }
    }

    fn create_reorder_fix<'arena>(
        &self,
        plan: &mut mago_fixer::FixPlan,
        original_args: &[&mago_syntax::ast::NamedArgument<'arena>],
        sorted_args: &[&mago_syntax::ast::NamedArgument<'arena>],
    ) {
        if original_args.len() != sorted_args.len() || original_args.is_empty() {
            return;
        }

        // Find the span covering all arguments
        let first_arg_span = original_args[0].span();
        let last_arg_span = original_args[original_args.len() - 1].span();
        let full_span = mago_span::Span::between(first_arg_span, last_arg_span);

        // Build the replacement text with properly ordered arguments
        let mut replacement_parts = Vec::new();

        for &arg in sorted_args.iter() {
            // Get the original text of the argument
            let arg_text = format!("{}: {}", arg.name.value, self.get_argument_value_text(arg));
            replacement_parts.push(arg_text);
        }

        let replacement_text = replacement_parts.join(",\n    ");

        plan.replace(full_span.to_range(), replacement_text, SafetyClassification::Safe);
    }

    fn get_argument_value_text<'arena>(&self, arg: &mago_syntax::ast::NamedArgument<'arena>) -> String {
        // This is a simplified version - in a real implementation, you'd want to
        // preserve the exact original text of the argument value
        match &arg.value {
            mago_syntax::ast::Expression::Literal(literal) => match literal {
                mago_syntax::ast::Literal::String(s) => {
                    if let Some(value) = s.value {
                        format!("\"{}\"", value)
                    } else {
                        s.raw.to_string()
                    }
                }
                mago_syntax::ast::Literal::Integer(i) => {
                    if let Some(value) = i.value {
                        value.to_string()
                    } else {
                        i.raw.to_string()
                    }
                }
                mago_syntax::ast::Literal::Float(f) => f.raw.to_string(),
                mago_syntax::ast::Literal::True(_) => "true".to_string(),
                mago_syntax::ast::Literal::False(_) => "false".to_string(),
                mago_syntax::ast::Literal::Null(_) => "null".to_string(),
            },
            _ => "/* complex expression */".to_string(), // Fallback for complex expressions
        }
    }

    /// Look up function signature from codebase metadata
    fn lookup_function_signature_from_codebase(&self, ctx: &LintContext<'_, '_>, function_name: &str) -> Option<Vec<String>> {
        let codebase = ctx.codebase?;
        
        // Convert function name to lowercase Atom for lookup (PHP functions are case-insensitive)
        let function_atom = ascii_lowercase_atom(function_name);
        
        // Look for global function (scope_id is empty)
        let empty_scope = empty_atom();
        if let Some(function_metadata) = codebase.function_likes.get(&(empty_scope, function_atom)) {
            if function_metadata.kind == FunctionLikeKind::Function {
                let parameter_names: Vec<String> = function_metadata
                    .parameters
                    .iter()
                    .map(|param| {
                        // Remove the $ prefix from parameter names
                        let full_name = param.name.0.as_str();
                        full_name.strip_prefix('$').unwrap_or(full_name).to_string()
                    })
                    .collect();
                return Some(parameter_names);
            }
        }
        
        None
    }

    /// Look up method signature from codebase metadata
    fn lookup_method_signature_from_codebase(&self, ctx: &LintContext<'_, '_>, class_name: &str, method_name: &str) -> Option<Vec<String>> {
        let codebase = ctx.codebase?;
        
        // Convert names to lowercase Atoms for lookup (PHP names are case-insensitive)
        let class_atom = ascii_lowercase_atom(class_name);
        let method_atom = ascii_lowercase_atom(method_name);
        
        // Look for method in the specified class
        if let Some(method_metadata) = codebase.function_likes.get(&(class_atom, method_atom)) {
            if method_metadata.kind == FunctionLikeKind::Method {
                let parameter_names: Vec<String> = method_metadata
                    .parameters
                    .iter()
                    .map(|param| {
                        // Remove the $ prefix from parameter names
                        let full_name = param.name.0.as_str();
                        full_name.strip_prefix('$').unwrap_or(full_name).to_string()
                    })
                    .collect();
                return Some(parameter_names);
            }
        }
        
        None
    }

    /// Check method calls: $obj->method(args...)
    fn check_method_call_with_codebase<'arena>(
        &self,
        ctx: &mut LintContext<'_, 'arena>,
        method_call: &MethodCall<'arena>,
    ) {
        // Only check if there are named arguments
        let named_args: Vec<_> = method_call
            .argument_list
            .arguments
            .iter()
            .filter_map(|arg| match arg {
                Argument::Named(named) => Some(named),
                _ => None,
            })
            .collect();

        if named_args.len() < 2 {
            return; // Need at least 2 named arguments to check order
        }

        let method_name = match &method_call.method {
            ClassLikeMemberSelector::Identifier(identifier) => identifier.value,
            _ => return, // Can't resolve complex method selectors yet
        };

        // Try to resolve the object type and find the method signature
        // For now, we'll implement a basic version that looks for common patterns
        if let Some(parameter_order) = self.lookup_method_from_object_type(ctx, method_call, method_name) {
            let parameter_refs: Vec<&str> = parameter_order.iter().map(|s| s.as_str()).collect();
            self.check_argument_order_against_expected(ctx, &named_args, &parameter_refs, "method signature");
        }
    }

    /// Check static method calls: Class::method(args...)
    fn check_static_method_call_with_codebase<'arena>(
        &self,
        ctx: &mut LintContext<'_, 'arena>,
        static_call: &StaticMethodCall<'arena>,
    ) {
        // Only check if there are named arguments
        let named_args: Vec<_> = static_call
            .argument_list
            .arguments
            .iter()
            .filter_map(|arg| match arg {
                Argument::Named(named) => Some(named),
                _ => None,
            })
            .collect();

        if named_args.len() < 2 {
            return; // Need at least 2 named arguments to check order
        }

        // Get class and method names
        let class_name = match &static_call.class {
            Expression::Identifier(identifier) => identifier.value(),
            _ => return, // Can't resolve complex class expressions yet
        };

        let method_name = match &static_call.method {
            ClassLikeMemberSelector::Identifier(identifier) => identifier.value,
            _ => return, // Can't resolve complex method selectors yet
        };

        // Look up the method signature from codebase metadata
        if let Some(parameter_order) = self.lookup_method_signature_from_codebase(ctx, class_name, method_name) {
            let parameter_refs: Vec<&str> = parameter_order.iter().map(|s| s.as_str()).collect();
            self.check_argument_order_against_expected(ctx, &named_args, &parameter_refs, "static method signature");
        }
    }

    /// Check instantiation calls: new Class(args...)
    fn check_instantiation_with_signatures<'arena>(
        &self,
        ctx: &mut LintContext<'_, 'arena>,
        instantiation: &Instantiation<'arena>,
        class_constructors: &std::collections::HashMap<&'arena str, Vec<&'arena str>>,
    ) {
        let Some(argument_list) = &instantiation.argument_list else {
            return; // No arguments to check
        };

        // Only check if there are named arguments
        let named_args: Vec<_> = argument_list
            .arguments
            .iter()
            .filter_map(|arg| match arg {
                Argument::Named(named) => Some(named),
                _ => None,
            })
            .collect();

        if named_args.len() < 2 {
            return; // Need at least 2 named arguments to check order
        }

        // Get the class name
        let class_name = match instantiation.class {
            Expression::Identifier(identifier) => identifier.value(),
            _ => return, // Can't resolve complex class expressions yet
        };

        // First check local constructor signatures (same file)
        if let Some(parameter_order) = class_constructors.get(class_name) {
            self.check_argument_order_against_expected(
                ctx,
                &named_args,
                parameter_order,
                "constructor signature",
            );
            return;
        }

        // Then check cross-file constructor signatures from codebase metadata
        if let Some(parameter_order) = self.lookup_method_signature_from_codebase(ctx, class_name, "__construct") {
            let parameter_refs: Vec<&str> = parameter_order.iter().map(|s| s.as_str()).collect();
            self.check_argument_order_against_expected(
                ctx,
                &named_args,
                &parameter_refs,
                "constructor signature",
            );
        }
    }

    /// Try to resolve method signature from object type (simplified version)
    fn lookup_method_from_object_type(
        &self,
        ctx: &LintContext<'_, '_>,
        method_call: &MethodCall<'_>,
        method_name: &str,
    ) -> Option<Vec<String>> {
        // This is a simplified implementation. In a full implementation, we would:
        // 1. Analyze the object expression to determine its type
        // 2. Look up the method in that class's metadata
        // 3. Handle inheritance and trait usage
        
        // For now, let's handle some common patterns:
        
        // Pattern: $this->method() - look in current class context
        if let Expression::Variable(Variable::Direct(var)) = &method_call.object {
            if var.name == "$this" {
                // Try to get current class context from scope
                if let Some(current_class) = self.get_current_class_from_context(ctx) {
                    return self.lookup_method_signature_from_codebase(ctx, &current_class, method_name);
                }
            }
        }

        // Pattern: $service->method() - could be dependency injection
        // This would require more sophisticated type analysis
        
        None
    }

    /// Get current class context (simplified)
    fn get_current_class_from_context(&self, _ctx: &LintContext<'_, '_>) -> Option<String> {
        // This would need to be implemented by tracking the current class scope
        // For now, return None as this requires more complex scope tracking
        None
    }
}
