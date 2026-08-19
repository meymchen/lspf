#![cfg(not(target_arch = "wasm32"))]

//! ADR 0020: the protocol engine reaches an executor only through the internal
//! `Runtime` trait, so the native and WASM send models stay a compile-target
//! choice rather than a call site scattered through the core.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use syn::visit::Visit;

/// Native Transport files allowed to reach Tokio directly. Runtime adapter
/// exemptions are narrower: only the `TokioRuntime` and `WasmRuntime` impl
/// bodies receive them.
const ALLOWED_NATIVE_TRANSPORT_BOUNDARIES: [&str; 3] = [
    "transport/stdio.rs",
    "transport/tcp.rs",
    "transport/websocket.rs",
];

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    fn collect(directory: &Path, files: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", directory.display()))
        {
            let entry = entry.expect("a source entry is readable");
            let file_type = entry.file_type().expect("a source entry has a file type");
            let path = entry.path();
            if file_type.is_dir() {
                collect(&path, files);
            } else if file_type.is_file()
                && path.extension().is_some_and(|extension| extension == "rs")
            {
                files.push(path);
            }
        }
    }

    let mut files = Vec::new();
    collect(root, &mut files);
    files.sort();
    files
}

fn is_test_only(attributes: &[syn::Attribute]) -> bool {
    fn requires_test(meta: &syn::Meta) -> bool {
        match meta {
            syn::Meta::Path(path) => path.is_ident("test"),
            syn::Meta::List(list) if list.path.is_ident("all") => list
                .parse_args_with(
                    syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                )
                .is_ok_and(|items| items.iter().any(requires_test)),
            syn::Meta::List(list) if list.path.is_ident("any") => list
                .parse_args_with(
                    syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                )
                .is_ok_and(|items| !items.is_empty() && items.iter().all(requires_test)),
            _ => false,
        }
    }

    attributes.iter().any(|attribute| {
        attribute.path().is_ident("test")
            || (attribute.path().is_ident("cfg")
                && attribute
                    .parse_args::<syn::Meta>()
                    .is_ok_and(|meta| requires_test(&meta)))
    })
}

fn path_name(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

fn use_tree_exports(tree: &syn::UseTree, expected: &str) -> bool {
    match tree {
        syn::UseTree::Path(path) => use_tree_exports(&path.tree, expected),
        syn::UseTree::Name(name) => name.ident == expected,
        syn::UseTree::Rename(rename) => rename.rename == expected,
        syn::UseTree::Group(group) => group
            .items
            .iter()
            .any(|item| use_tree_exports(item, expected)),
        syn::UseTree::Glob(_) => false,
    }
}

fn cfg_matches_native_feature(attribute: &syn::Attribute, expected: &str) -> bool {
    fn nested(meta: &syn::Meta) -> Vec<syn::Meta> {
        let syn::Meta::List(list) = meta else {
            return Vec::new();
        };
        list.parse_args_with(
            syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
        )
        .map(|items| items.into_iter().collect())
        .unwrap_or_default()
    }

    fn is_name_value(meta: &syn::Meta, name: &str, value: &str) -> bool {
        let syn::Meta::NameValue(name_value) = meta else {
            return false;
        };
        let syn::Expr::Lit(expression) = &name_value.value else {
            return false;
        };
        name_value.path.is_ident(name)
            && matches!(&expression.lit, syn::Lit::Str(literal) if literal.value() == value)
    }

    if !attribute.path().is_ident("cfg") {
        return false;
    }
    let Ok(meta) = attribute.parse_args::<syn::Meta>() else {
        return false;
    };
    let clauses = nested(&meta);
    let has_feature = clauses
        .iter()
        .any(|clause| is_name_value(clause, "feature", expected));
    let excludes_wasm = clauses.iter().any(|clause| {
        matches!(clause, syn::Meta::List(list) if list.path.is_ident("not"))
            && nested(clause)
                .iter()
                .any(|inner| is_name_value(inner, "target_arch", "wasm32"))
    });
    has_feature && excludes_wasm
}

fn is_executor_spawn(path: &syn::Path, executor_roots: &HashSet<String>) -> bool {
    let mut segments = path.segments.iter();
    let Some(first) = segments.next() else {
        return false;
    };
    let Some(last) = path.segments.last() else {
        return false;
    };
    let spawn_function = matches!(
        last.ident.to_string().as_str(),
        "spawn" | "spawn_blocking" | "spawn_local"
    );

    spawn_function && executor_roots.contains(&first.ident.to_string())
}

fn executor_import_findings(tree: &syn::UseTree, executor_roots: &HashSet<String>) -> Vec<String> {
    fn collect(
        tree: &syn::UseTree,
        prefix: &mut Vec<String>,
        findings: &mut Vec<String>,
        executor_roots: &HashSet<String>,
    ) {
        match tree {
            syn::UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                collect(&path.tree, prefix, findings, executor_roots);
                prefix.pop();
            }
            syn::UseTree::Name(name) => {
                prefix.push(name.ident.to_string());
                let root = prefix.first().map(String::as_str);
                let leaf = prefix.last().map(String::as_str);
                if root.is_some_and(|root| executor_roots.contains(root))
                    && matches!(
                        leaf,
                        Some(
                            "spawn"
                                | "spawn_blocking"
                                | "spawn_local"
                                | "Handle"
                                | "runtime"
                                | "task"
                        )
                    )
                {
                    findings.push(prefix.join("::"));
                }
                prefix.pop();
            }
            syn::UseTree::Rename(rename) => {
                prefix.push(rename.ident.to_string());
                let root = prefix.first().map(String::as_str);
                let leaf = prefix.last().map(String::as_str);
                if root.is_some_and(|root| executor_roots.contains(root))
                    && (prefix.len() == 1
                        || matches!(
                            leaf,
                            Some(
                                "spawn"
                                    | "spawn_blocking"
                                    | "spawn_local"
                                    | "Handle"
                                    | "runtime"
                                    | "task"
                            )
                        ))
                {
                    findings.push(format!("{} as {}", prefix.join("::"), rename.rename));
                }
                prefix.pop();
            }
            syn::UseTree::Glob(_) => {
                if prefix
                    .first()
                    .is_some_and(|root| executor_roots.contains(root))
                {
                    findings.push(format!("{}::*", prefix.join("::")));
                }
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    collect(item, prefix, findings, executor_roots);
                }
            }
        }
    }

    let mut findings = Vec::new();
    collect(tree, &mut Vec::new(), &mut findings, executor_roots);
    findings
}

fn executor_import_names(
    tree: &syn::UseTree,
    executor_roots: &HashSet<String>,
) -> (HashSet<String>, HashSet<String>) {
    fn collect(
        tree: &syn::UseTree,
        prefix: &mut Vec<String>,
        executor_roots: &HashSet<String>,
        namespaces: &mut HashSet<String>,
        handle_types: &mut HashSet<String>,
    ) {
        match tree {
            syn::UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                collect(&path.tree, prefix, executor_roots, namespaces, handle_types);
                prefix.pop();
            }
            syn::UseTree::Name(name) => {
                if prefix
                    .first()
                    .is_some_and(|root| executor_roots.contains(root))
                {
                    let imported = name.ident.to_string();
                    namespaces.insert(imported.clone());
                    if imported == "Handle" {
                        handle_types.insert(imported);
                    }
                }
            }
            syn::UseTree::Rename(rename) => {
                if prefix
                    .first()
                    .is_some_and(|root| executor_roots.contains(root))
                {
                    let imported = rename.rename.to_string();
                    namespaces.insert(imported.clone());
                    if rename.ident == "Handle" {
                        handle_types.insert(imported);
                    }
                }
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    collect(item, prefix, executor_roots, namespaces, handle_types);
                }
            }
            syn::UseTree::Glob(_) => {}
        }
    }

    let mut namespaces = HashSet::new();
    let mut handle_types = HashSet::new();
    collect(
        tree,
        &mut Vec::new(),
        executor_roots,
        &mut namespaces,
        &mut handle_types,
    );
    (namespaces, handle_types)
}

fn executor_path_in(expression: &syn::Expr, executor_roots: &HashSet<String>) -> Option<String> {
    struct ExecutorPathVisitor<'a> {
        found: Option<String>,
        executor_roots: &'a HashSet<String>,
    }

    impl<'ast> Visit<'ast> for ExecutorPathVisitor<'_> {
        fn visit_expr_path(&mut self, expression: &'ast syn::ExprPath) {
            if self.found.is_none()
                && matches!(
                    expression.path.segments.first().map(|segment| &segment.ident),
                    Some(ident) if self.executor_roots.contains(&ident.to_string())
                )
            {
                self.found = Some(path_name(&expression.path));
            }
            syn::visit::visit_expr_path(self, expression);
        }
    }

    let mut visitor = ExecutorPathVisitor {
        found: None,
        executor_roots,
    };
    visitor.visit_expr(expression);
    visitor.found
}

fn type_is_executor_handle(
    kind: &syn::Type,
    executor_roots: &HashSet<String>,
    executor_handle_types: &HashSet<String>,
) -> bool {
    struct HandleTypeVisitor<'a> {
        found: bool,
        executor_roots: &'a HashSet<String>,
        executor_handle_types: &'a HashSet<String>,
    }

    impl<'ast> Visit<'ast> for HandleTypeVisitor<'_> {
        fn visit_type_path(&mut self, kind: &'ast syn::TypePath) {
            let first = kind.path.segments.first();
            if (matches!(first, Some(segment)
                if self.executor_roots.contains(&segment.ident.to_string()))
                && kind
                    .path
                    .segments
                    .iter()
                    .any(|segment| segment.ident == "Handle"))
                || (kind.path.segments.len() == 1
                    && first.is_some_and(|segment| {
                        self.executor_handle_types
                            .contains(&segment.ident.to_string())
                    }))
            {
                self.found = true;
            }
            syn::visit::visit_type_path(self, kind);
        }
    }

    let mut visitor = HandleTypeVisitor {
        found: false,
        executor_roots,
        executor_handle_types,
    };
    visitor.visit_type(kind);
    visitor.found
}

fn executor_handle_returning_name(
    attributes: &[syn::Attribute],
    signature: &syn::Signature,
    executor_roots: &HashSet<String>,
    executor_handle_types: &HashSet<String>,
) -> Option<String> {
    (!is_test_only(attributes)
        && matches!(&signature.output, syn::ReturnType::Type(_, kind)
            if type_is_executor_handle(kind, executor_roots, executor_handle_types)))
    .then(|| signature.ident.to_string())
}

fn expression_references_binding(expression: &syn::Expr, bindings: &HashSet<String>) -> bool {
    struct BindingVisitor<'a> {
        bindings: &'a HashSet<String>,
        found: bool,
    }

    impl<'ast> Visit<'ast> for BindingVisitor<'_> {
        fn visit_expr_path(&mut self, expression: &'ast syn::ExprPath) {
            if expression.path.segments.len() == 1
                && self
                    .bindings
                    .contains(&expression.path.segments[0].ident.to_string())
            {
                self.found = true;
            }
            syn::visit::visit_expr_path(self, expression);
        }
    }

    let mut visitor = BindingVisitor {
        bindings,
        found: false,
    };
    visitor.visit_expr(expression);
    visitor.found
}

fn token_identifiers(tokens: proc_macro2::TokenStream) -> Vec<String> {
    fn collect(tokens: proc_macro2::TokenStream, identifiers: &mut Vec<String>) {
        for token in tokens {
            match token {
                proc_macro2::TokenTree::Ident(identifier) => {
                    identifiers.push(identifier.to_string());
                }
                proc_macro2::TokenTree::Group(group) => collect(group.stream(), identifiers),
                _ => {}
            }
        }
    }

    let mut identifiers = Vec::new();
    collect(tokens, &mut identifiers);
    identifiers
}

fn member_name(member: &syn::Member) -> String {
    match member {
        syn::Member::Named(name) => name.to_string(),
        syn::Member::Unnamed(index) => index.index.to_string(),
    }
}

struct ExecutorProvenance {
    roots: HashSet<String>,
    handle_types: HashSet<String>,
    handle_fields: HashSet<(String, String)>,
    handle_functions: HashSet<String>,
    handle_methods: HashSet<String>,
    owner_fields: std::collections::HashMap<(String, String), String>,
}

fn executor_provenance(
    syntax: &syn::File,
    mut executor_roots: HashSet<String>,
) -> ExecutorProvenance {
    struct AliasCollector {
        executor_roots: HashSet<String>,
        handle_types: HashSet<String>,
    }

    impl<'ast> Visit<'ast> for AliasCollector {
        fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
            if !is_test_only(&module.attrs) {
                syn::visit::visit_item_mod(self, module);
            }
        }

        fn visit_item_type(&mut self, item: &'ast syn::ItemType) {
            if !is_test_only(&item.attrs)
                && type_is_executor_handle(&item.ty, &self.executor_roots, &self.handle_types)
            {
                self.handle_types.insert(item.ident.to_string());
            }
            syn::visit::visit_item_type(self, item);
        }

        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            if !is_test_only(&item.attrs) {
                let (namespaces, handle_types) =
                    executor_import_names(&item.tree, &self.executor_roots);
                self.executor_roots.extend(namespaces);
                self.handle_types.extend(handle_types);
            }
            syn::visit::visit_item_use(self, item);
        }
    }

    let mut handle_types = HashSet::new();
    loop {
        let mut collector = AliasCollector {
            executor_roots: executor_roots.clone(),
            handle_types: handle_types.clone(),
        };
        collector.visit_file(syntax);
        if collector.executor_roots == executor_roots && collector.handle_types == handle_types {
            break;
        }
        executor_roots = collector.executor_roots;
        handle_types = collector.handle_types;
    }

    struct DeclarationCollector<'a> {
        executor_roots: &'a HashSet<String>,
        handle_types: &'a HashSet<String>,
        handle_fields: HashSet<(String, String)>,
        handle_functions: HashSet<String>,
        handle_methods: HashSet<String>,
        owner_fields: std::collections::HashMap<(String, String), String>,
    }

    impl<'ast> Visit<'ast> for DeclarationCollector<'_> {
        fn visit_item_fn(&mut self, function: &'ast syn::ItemFn) {
            if let Some(name) = executor_handle_returning_name(
                &function.attrs,
                &function.sig,
                self.executor_roots,
                self.handle_types,
            ) {
                self.handle_functions.insert(name);
            }
            syn::visit::visit_item_fn(self, function);
        }

        fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
            if !is_test_only(&module.attrs) {
                syn::visit::visit_item_mod(self, module);
            }
        }

        fn visit_impl_item_fn(&mut self, function: &'ast syn::ImplItemFn) {
            if let Some(name) = executor_handle_returning_name(
                &function.attrs,
                &function.sig,
                self.executor_roots,
                self.handle_types,
            ) {
                self.handle_methods.insert(name);
            }
            syn::visit::visit_impl_item_fn(self, function);
        }

        fn visit_trait_item_fn(&mut self, function: &'ast syn::TraitItemFn) {
            if let Some(name) = executor_handle_returning_name(
                &function.attrs,
                &function.sig,
                self.executor_roots,
                self.handle_types,
            ) {
                self.handle_methods.insert(name);
            }
            syn::visit::visit_trait_item_fn(self, function);
        }

        fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
            if !is_test_only(&item.attrs) {
                for (index, field) in item.fields.iter().enumerate() {
                    let member = field
                        .ident
                        .as_ref()
                        .map_or_else(|| index.to_string(), ToString::to_string);
                    if type_is_executor_handle(&field.ty, self.executor_roots, self.handle_types) {
                        self.handle_fields
                            .insert((item.ident.to_string(), member.clone()));
                    } else if let syn::Type::Path(kind) = &field.ty
                        && let Some(owner) = kind.path.segments.last()
                    {
                        self.owner_fields
                            .insert((item.ident.to_string(), member), owner.ident.to_string());
                    }
                }
            }
            syn::visit::visit_item_struct(self, item);
        }
    }

    let mut declarations = DeclarationCollector {
        executor_roots: &executor_roots,
        handle_types: &handle_types,
        handle_fields: HashSet::new(),
        handle_functions: HashSet::new(),
        handle_methods: HashSet::new(),
        owner_fields: std::collections::HashMap::new(),
    };
    declarations.visit_file(syntax);
    let handle_fields = std::mem::take(&mut declarations.handle_fields);
    let handle_functions = std::mem::take(&mut declarations.handle_functions);
    let handle_methods = std::mem::take(&mut declarations.handle_methods);
    let owner_fields = std::mem::take(&mut declarations.owner_fields);
    drop(declarations);
    ExecutorProvenance {
        roots: executor_roots,
        handle_types,
        handle_fields,
        handle_functions,
        handle_methods,
        owner_fields,
    }
}

struct BoundaryVisitor {
    executor_spawns: Vec<String>,
    unsafe_send_sync_impls: Vec<String>,
    executor_handle_bindings: HashSet<String>,
    executor_handle_types: HashSet<String>,
    executor_handle_fields: HashSet<(String, String)>,
    executor_handle_functions: HashSet<String>,
    executor_handle_methods: HashSet<String>,
    executor_owner_fields: std::collections::HashMap<(String, String), String>,
    current_impl_type: Option<String>,
    executor_roots: HashSet<String>,
    send_sync_names: HashSet<String>,
    allow_runtime_adapters: bool,
    allow_executor_types: bool,
    runtime_adapter_depth: usize,
}

impl BoundaryVisitor {
    fn expression_owner(&self, expression: &syn::Expr) -> Option<String> {
        match expression {
            syn::Expr::Path(path) if path.path.is_ident("self") => self.current_impl_type.clone(),
            syn::Expr::Field(field) => {
                let owner = self.expression_owner(&field.base)?;
                self.executor_owner_fields
                    .get(&(owner, member_name(&field.member)))
                    .cloned()
            }
            syn::Expr::Paren(parenthesized) => self.expression_owner(&parenthesized.expr),
            syn::Expr::Reference(reference) => self.expression_owner(&reference.expr),
            _ => None,
        }
    }

    fn field_is_executor_handle(&self, field: &syn::ExprField) -> bool {
        self.expression_owner(&field.base).is_some_and(|owner| {
            self.executor_handle_fields
                .contains(&(owner, member_name(&field.member)))
        })
    }

    fn expression_is_executor_handle(&self, expression: &syn::Expr) -> bool {
        match expression {
            syn::Expr::Path(path) if path.path.segments.len() == 1 => self
                .executor_handle_bindings
                .contains(&path.path.segments[0].ident.to_string()),
            syn::Expr::Call(call) => {
                executor_path_in(expression, &self.executor_roots)
                    .is_some_and(|path| path.split("::").any(|segment| segment == "Handle"))
                    || matches!(call.func.as_ref(), syn::Expr::Path(function)
                    if function.path.segments.len() == 1
                        && self.executor_handle_bindings.contains(
                            &function.path.segments[0].ident.to_string(),
                        ))
                    || matches!(call.func.as_ref(), syn::Expr::Path(function)
                    if function.path.segments.last().is_some_and(|segment| {
                        self.executor_handle_functions.contains(&segment.ident.to_string())
                            || self
                                .executor_handle_methods
                                .contains(&segment.ident.to_string())
                    }))
            }
            syn::Expr::Field(field) => self.field_is_executor_handle(field),
            syn::Expr::MethodCall(call) => {
                self.executor_handle_methods
                    .contains(&call.method.to_string())
                    || (call.method == "clone"
                        && self.expression_is_executor_handle(&call.receiver))
            }
            syn::Expr::Closure(closure) => {
                matches!(&closure.output, syn::ReturnType::Type(_, kind)
                if type_is_executor_handle(
                    kind,
                    &self.executor_roots,
                    &self.executor_handle_types,
                )) || self.expression_is_executor_handle(&closure.body)
            }
            syn::Expr::Block(block) => block.block.stmts.iter().any(|statement| match statement {
                syn::Stmt::Local(local) => local.init.as_ref().is_some_and(|initializer| {
                    self.expression_is_executor_handle(&initializer.expr)
                }),
                syn::Stmt::Expr(expression, _) => self.expression_is_executor_handle(expression),
                syn::Stmt::Item(_) | syn::Stmt::Macro(_) => false,
            }),
            syn::Expr::Return(returned) => returned
                .expr
                .as_ref()
                .is_some_and(|expression| self.expression_is_executor_handle(expression)),
            syn::Expr::Paren(parenthesized) => {
                self.expression_is_executor_handle(&parenthesized.expr)
            }
            syn::Expr::Reference(reference) => self.expression_is_executor_handle(&reference.expr),
            _ => false,
        }
    }

    fn visit_function_scope(
        &mut self,
        inputs: &syn::punctuated::Punctuated<syn::FnArg, syn::Token![,]>,
        visit: impl FnOnce(&mut Self),
    ) {
        let outer_bindings = std::mem::take(&mut self.executor_handle_bindings);
        for argument in inputs {
            if let syn::FnArg::Typed(argument) = argument
                && type_is_executor_handle(
                    &argument.ty,
                    &self.executor_roots,
                    &self.executor_handle_types,
                )
                && let syn::Pat::Ident(pattern) = argument.pat.as_ref()
            {
                self.executor_handle_bindings
                    .insert(pattern.ident.to_string());
            }
        }
        visit(self);
        self.executor_handle_bindings = outer_bindings;
    }
}

impl<'ast> Visit<'ast> for BoundaryVisitor {
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(function) = call.func.as_ref()
            && is_executor_spawn(&function.path, &self.executor_roots)
            && self.runtime_adapter_depth == 0
        {
            self.executor_spawns.push(path_name(&function.path));
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if self.runtime_adapter_depth == 0
            && matches!(
                call.method.to_string().as_str(),
                "spawn" | "spawn_blocking" | "spawn_local"
            )
            && self.expression_is_executor_handle(&call.receiver)
        {
            self.executor_spawns
                .push(format!("executor handle::{}", call.method));
        }
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        if self.runtime_adapter_depth == 0
            && let Some(initializer) = &local.init
            && let Some((binding, typed_as_executor)) = match &local.pat {
                syn::Pat::Ident(pattern) => Some((pattern.ident.to_string(), false)),
                syn::Pat::Type(pattern) => {
                    let syn::Pat::Ident(binding) = pattern.pat.as_ref() else {
                        return syn::visit::visit_local(self, local);
                    };
                    Some((
                        binding.ident.to_string(),
                        type_is_executor_handle(
                            &pattern.ty,
                            &self.executor_roots,
                            &self.executor_handle_types,
                        ),
                    ))
                }
                _ => None,
            }
        {
            if let syn::Expr::Path(function) = initializer.expr.as_ref()
                && is_executor_spawn(&function.path, &self.executor_roots)
            {
                self.executor_spawns
                    .push(format!("{} as {binding}", path_name(&function.path)));
            }
            if typed_as_executor
                || self.expression_is_executor_handle(&initializer.expr)
                || expression_references_binding(&initializer.expr, &self.executor_handle_bindings)
            {
                self.executor_handle_bindings.insert(binding);
            }
        }
        syn::visit::visit_local(self, local);
    }

    fn visit_item_extern_crate(&mut self, item: &'ast syn::ItemExternCrate) {
        if self.executor_roots.contains(&item.ident.to_string())
            && let Some((_, rename)) = &item.rename
        {
            self.executor_spawns
                .push(format!("extern crate {} as {}", item.ident, rename));
        }
        syn::visit::visit_item_extern_crate(self, item);
    }

    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        syn::visit::visit_item_struct(self, item);
    }

    fn visit_item_type(&mut self, item: &'ast syn::ItemType) {
        if type_is_executor_handle(&item.ty, &self.executor_roots, &self.executor_handle_types) {
            self.executor_handle_types.insert(item.ident.to_string());
        }
        syn::visit::visit_item_type(self, item);
    }

    fn visit_type_path(&mut self, kind: &'ast syn::TypePath) {
        if !self.allow_executor_types
            && type_is_executor_handle(
                &syn::Type::Path(kind.clone()),
                &self.executor_roots,
                &self.executor_handle_types,
            )
        {
            self.executor_spawns
                .push(format!("executor handle type::{}", path_name(&kind.path)));
        }
        syn::visit::visit_type_path(self, kind);
    }

    fn visit_item_fn(&mut self, function: &'ast syn::ItemFn) {
        if !is_test_only(&function.attrs) {
            self.visit_function_scope(&function.sig.inputs, |visitor| {
                syn::visit::visit_item_fn(visitor, function);
            });
        }
    }

    fn visit_impl_item_fn(&mut self, function: &'ast syn::ImplItemFn) {
        if !is_test_only(&function.attrs) {
            self.visit_function_scope(&function.sig.inputs, |visitor| {
                syn::visit::visit_impl_item_fn(visitor, function);
            });
        }
    }

    fn visit_item_impl(&mut self, implementation: &'ast syn::ItemImpl) {
        if is_test_only(&implementation.attrs) {
            return;
        }
        if implementation.unsafety.is_some()
            && let Some((_, trait_path, _)) = &implementation.trait_
            && trait_path
                .segments
                .last()
                .is_some_and(|segment| self.send_sync_names.contains(&segment.ident.to_string()))
        {
            self.unsafe_send_sync_impls.push(path_name(trait_path));
        }

        let is_runtime_adapter = self.allow_runtime_adapters
            && implementation
                .trait_
                .as_ref()
                .and_then(|(_, path, _)| path.segments.last())
                .is_some_and(|segment| segment.ident == "Runtime")
            && matches!(
                implementation.self_ty.as_ref(),
                syn::Type::Path(type_path)
                    if type_path.path.segments.last().is_some_and(|segment| {
                        matches!(segment.ident.to_string().as_str(), "TokioRuntime" | "WasmRuntime")
                    })
            );
        let outer_impl_type = self.current_impl_type.take();
        self.current_impl_type = match implementation.self_ty.as_ref() {
            syn::Type::Path(kind) => kind
                .path
                .segments
                .last()
                .map(|segment| segment.ident.to_string()),
            _ => None,
        };
        self.runtime_adapter_depth += usize::from(is_runtime_adapter);
        syn::visit::visit_item_impl(self, implementation);
        self.runtime_adapter_depth -= usize::from(is_runtime_adapter);
        self.current_impl_type = outer_impl_type;
    }

    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        if !is_test_only(&module.attrs) {
            syn::visit::visit_item_mod(self, module);
        }
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        let (namespaces, handle_types) = executor_import_names(&item.tree, &self.executor_roots);
        self.executor_roots.extend(namespaces);
        self.executor_handle_types.extend(handle_types);
        self.executor_spawns
            .extend(executor_import_findings(&item.tree, &self.executor_roots));
        syn::visit::visit_item_use(self, item);
    }

    fn visit_macro(&mut self, item: &'ast syn::Macro) {
        let identifiers = token_identifiers(item.tokens.clone());
        if self.runtime_adapter_depth == 0 {
            for executor in &self.executor_roots {
                if identifiers.iter().any(|identifier| identifier == executor) {
                    let spawn = identifiers
                        .iter()
                        .find(|identifier| {
                            matches!(
                                identifier.as_str(),
                                "spawn" | "spawn_blocking" | "spawn_local"
                            )
                        })
                        .map_or("parameterized spawn", String::as_str);
                    self.executor_spawns
                        .push(format!("macro {executor}::{spawn}"));
                }
            }
        }
        if identifiers.iter().any(|identifier| identifier == "unsafe")
            && identifiers.iter().any(|identifier| identifier == "impl")
        {
            let marker = identifiers
                .iter()
                .find(|identifier| self.send_sync_names.contains(identifier.as_str()))
                .map_or("parameterized Send/Sync", String::as_str);
            self.unsafe_send_sync_impls.push(format!("macro {marker}"));
        }
        syn::visit::visit_macro(self, item);
    }
}

fn send_sync_aliases(syntax: &syn::File) -> HashSet<String> {
    fn collect(tree: &syn::UseTree, aliases: &mut HashSet<String>) {
        match tree {
            syn::UseTree::Path(path) => collect(&path.tree, aliases),
            syn::UseTree::Rename(rename)
                if matches!(rename.ident.to_string().as_str(), "Send" | "Sync") =>
            {
                aliases.insert(rename.rename.to_string());
            }
            syn::UseTree::Group(group) => {
                for tree in &group.items {
                    collect(tree, aliases);
                }
            }
            _ => {}
        }
    }

    #[derive(Default)]
    struct AliasVisitor(HashSet<String>);

    impl<'ast> Visit<'ast> for AliasVisitor {
        fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
            if !is_test_only(&module.attrs) {
                syn::visit::visit_item_mod(self, module);
            }
        }

        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            collect(&item.tree, &mut self.0);
        }
    }

    let mut visitor = AliasVisitor::default();
    visitor.visit_file(syntax);
    visitor.0.extend(["Send".to_owned(), "Sync".to_owned()]);
    visitor.0
}

fn canonical_executor_roots() -> HashSet<String> {
    HashSet::from(["tokio".to_owned(), "wasm_bindgen_futures".to_owned()])
}

fn executor_dependency_roots() -> HashSet<String> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = std::process::Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(manifest_dir)
        .output()
        .expect("Cargo metadata is available to the structural test");
    assert!(
        output.status.success(),
        "Cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("Cargo metadata is valid JSON");
    let manifest_path = manifest_dir.join("Cargo.toml");
    let package = metadata["packages"]
        .as_array()
        .and_then(|packages| {
            packages.iter().find(|package| {
                package["manifest_path"].as_str().map(Path::new) == Some(manifest_path.as_path())
            })
        })
        .expect("Cargo metadata contains the lspf package");
    let roots = package["dependencies"]
        .as_array()
        .expect("Cargo metadata dependencies are an array")
        .iter()
        .filter(|dependency| {
            matches!(
                dependency["name"].as_str(),
                Some("tokio" | "wasm-bindgen-futures")
            )
        })
        .map(|dependency| {
            dependency["rename"]
                .as_str()
                .or_else(|| dependency["name"].as_str())
                .expect("an executor dependency has a source name")
                .replace('-', "_")
        })
        .collect::<HashSet<_>>();
    assert_eq!(
        roots.len(),
        2,
        "the structural guard must discover both executor dependencies"
    );
    roots
}

fn inspect_source_with_configuration(
    path: &Path,
    allow_runtime_adapters: bool,
    executor_roots: HashSet<String>,
) -> BoundaryVisitor {
    let source = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    let syntax = syn::parse_file(&source)
        .unwrap_or_else(|error| panic!("cannot parse {} as Rust: {error}", path.display()));
    let provenance = executor_provenance(&syntax, executor_roots);
    let mut visitor = BoundaryVisitor {
        executor_spawns: Vec::new(),
        unsafe_send_sync_impls: Vec::new(),
        executor_handle_bindings: HashSet::new(),
        executor_handle_types: provenance.handle_types,
        executor_handle_fields: provenance.handle_fields,
        executor_handle_functions: provenance.handle_functions,
        executor_handle_methods: provenance.handle_methods,
        executor_owner_fields: provenance.owner_fields,
        current_impl_type: None,
        executor_roots: provenance.roots,
        send_sync_names: send_sync_aliases(&syntax),
        allow_runtime_adapters,
        allow_executor_types: allow_runtime_adapters,
        runtime_adapter_depth: 0,
    };
    visitor.visit_file(&syntax);
    visitor
}

fn inspect_source_with_runtime_adapters(
    path: &Path,
    allow_runtime_adapters: bool,
) -> BoundaryVisitor {
    inspect_source_with_configuration(path, allow_runtime_adapters, canonical_executor_roots())
}

fn inspect_source_with_executor_roots(
    path: &Path,
    executor_roots: HashSet<String>,
) -> BoundaryVisitor {
    inspect_source_with_configuration(path, false, executor_roots)
}

fn inspect_source(path: &Path) -> BoundaryVisitor {
    inspect_source_with_runtime_adapters(path, false)
}

#[test]
fn the_protocol_kernel_routes_task_creation_through_runtime() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let executor_roots = executor_dependency_roots();
    for path in rust_sources(&source_root) {
        let relative = path
            .strip_prefix(&source_root)
            .expect("a source path is below the source root");
        if ALLOWED_NATIVE_TRANSPORT_BOUNDARIES.contains(&relative.to_string_lossy().as_ref()) {
            continue;
        }
        let findings = inspect_source_with_configuration(
            &path,
            relative == Path::new("runtime.rs"),
            executor_roots.clone(),
        );
        assert!(
            findings.executor_spawns.is_empty(),
            "{} reaches an executor directly ({}) instead of routing through Runtime",
            relative.display(),
            findings.executor_spawns.join(", ")
        );
    }
}

#[test]
fn spawn_guard_recognizes_rust_syntax_independent_of_whitespace() {
    let directory = tempfile::tempdir().expect("a temporary source directory");
    let source = directory.path().join("spaced.rs");
    std::fs::write(
        &source,
        "fn bypass() { tokio :: spawn(async {}); tokio::task::spawn_blocking(|| {}); \
         wasm_bindgen_futures::spawn_local(async {}); }\n\
         #[cfg(all(test, not(target_arch = \"wasm32\")))]\n\
         mod tests { fn allowed() { tokio::spawn(async {}); } }",
    )
    .expect("the source probe is writable");

    assert_eq!(
        inspect_source(&source).executor_spawns,
        [
            "tokio::spawn",
            "tokio::task::spawn_blocking",
            "wasm_bindgen_futures::spawn_local"
        ]
    );
}

#[test]
fn spawn_guard_limits_runtime_exemption_to_runtime_adapter_impls() {
    let directory = tempfile::tempdir().expect("a temporary source directory");
    let source = directory.path().join("runtime.rs");
    std::fs::write(
        &source,
        "trait Runtime {} struct TokioRuntime; \
         impl Runtime for TokioRuntime { fn spawn() { tokio::spawn(async {}); } } \
         fn bypass() { wasm_bindgen_futures::spawn_local(async {}); }",
    )
    .expect("the source probe is writable");

    assert_eq!(
        inspect_source_with_runtime_adapters(&source, true).executor_spawns,
        ["wasm_bindgen_futures::spawn_local"]
    );

    let fake_runtime = directory.path().join("fake_runtime.rs");
    std::fs::write(
        &fake_runtime,
        "trait Runtime {} struct TokioRuntime; \
         impl Runtime for TokioRuntime { fn spawn() { tokio::spawn(async {}); } }",
    )
    .expect("the fake runtime source probe is writable");
    assert_eq!(
        inspect_source(&fake_runtime).executor_spawns,
        ["tokio::spawn"]
    );
}

#[test]
fn spawn_guard_rejects_import_aliases_and_executor_method_calls() {
    let directory = tempfile::tempdir().expect("a temporary source directory");
    let source = directory.path().join("aliases.rs");
    std::fs::write(
        &source,
        "use tokio::spawn as launch; use tokio::runtime::Handle; use tokio::runtime; \
         type Executor = tokio::runtime::Handle; \
         struct Holder { handle: tokio::runtime::Handle } \
         impl Holder { fn bypass(&self) { self.handle.spawn(async {}); } } \
         struct MisnamedHolder { runtime: tokio::runtime::Handle } \
         impl MisnamedHolder { fn bypass(&self) { self.runtime.spawn(async {}); } } \
         fn parameter(parameter_handle: &Executor) { parameter_handle.spawn(async {}); } \
         fn bypass() { launch(async {}); Handle::current().spawn(async {}); \
         tokio::runtime::Handle::current().spawn(async {}); \
         let handle = tokio::runtime::Handle::current(); handle.spawn(async {}); \
         let cloned = handle.clone(); cloned.spawn(async {}); \
         let module_handle = runtime::Handle::current(); module_handle.spawn(async {}); \
         let local_launch = tokio::spawn; local_launch(async {}); }",
    )
    .expect("the source probe is writable");

    let findings = inspect_source(&source).executor_spawns;
    assert!(
        findings
            .iter()
            .any(|finding| finding.contains("spawn as launch"))
    );
    assert!(
        findings
            .iter()
            .any(|finding| finding == "tokio::runtime::Handle")
    );
    assert!(findings.iter().any(|finding| finding == "tokio::runtime"));
    assert!(
        findings
            .iter()
            .filter(|finding| *finding == "executor handle::spawn")
            .count()
            >= 8
    );
    assert!(
        findings
            .iter()
            .any(|finding| finding.contains("tokio::spawn as local_launch"))
    );
}

#[test]
fn spawn_guard_uses_executor_dependency_names_from_cargo_metadata() {
    let directory = tempfile::tempdir().expect("a temporary source directory");
    let source = directory.path().join("renamed_executor.rs");
    std::fs::write(
        &source,
        "fn bypass() { async_runtime::spawn(async {}); browser_tasks::spawn_local(async {}); }",
    )
    .expect("the source probe is writable");
    let executor_roots = HashSet::from(["async_runtime".to_owned(), "browser_tasks".to_owned()]);

    assert_eq!(
        inspect_source_with_executor_roots(&source, executor_roots).executor_spawns,
        ["async_runtime::spawn", "browser_tasks::spawn_local"]
    );
}

#[test]
fn spawn_guard_ignores_unrelated_domain_spawn_methods() {
    let directory = tempfile::tempdir().expect("a temporary source directory");
    let source = directory.path().join("domain_spawn.rs");
    std::fs::write(
        &source,
        "struct Child; impl Child { fn spawn(&self) {} } \
         struct Parent { child: Child } \
         impl Parent { fn launch(&self, child: Child) { child.spawn(); self.child.spawn(); } }",
    )
    .expect("the source probe is writable");

    assert!(inspect_source(&source).executor_spawns.is_empty());
}

#[test]
fn spawn_guard_rejects_executor_fields_independent_of_declaration_order() {
    let directory = tempfile::tempdir().expect("a temporary source directory");
    let source = directory.path().join("late_field.rs");
    std::fs::write(
        &source,
        "impl Holder { fn bypass(&self) { self.handle.spawn(async {}); } } \
         struct Holder { handle: tokio::runtime::Handle }",
    )
    .expect("the source probe is writable");

    assert!(
        inspect_source(&source)
            .executor_spawns
            .iter()
            .any(|finding| finding == "executor handle::spawn")
    );
}

#[test]
fn spawn_guard_rejects_executor_types_in_typed_bindings_and_tuple_fields() {
    let directory = tempfile::tempdir().expect("a temporary source directory");
    let source = directory.path().join("executor_types.rs");
    std::fs::write(
        &source,
        "fn obtain_handle() -> tokio::runtime::Handle { todo!() } \
         fn bypass() { let handle: tokio::runtime::Handle = obtain_handle(); \
         handle.spawn(async {}); } \
         fn direct() { obtain_handle().spawn(async {}); } \
         fn alias_bypass(handle: Executor) { handle.spawn(async {}); } \
         type Executor = tokio::runtime::Handle; \
         struct Holder(tokio::runtime::Handle); \
         impl Holder { fn bypass(&self) { self.0.spawn(async {}); \
         self.0.clone().spawn(async {}); } } \
         struct Inner { handle: tokio::runtime::Handle } \
         struct Outer { inner: Inner } \
         impl Outer { fn handle(&self) -> tokio::runtime::Handle { obtain_handle() } \
         fn bypass(&self) { self.handle().spawn(async {}); \
         self.inner.handle.spawn(async {}); } }",
    )
    .expect("the source probe is writable");

    let findings = inspect_source(&source).executor_spawns;
    assert!(
        findings
            .iter()
            .filter(|finding| *finding == "executor handle::spawn")
            .count()
            >= 7
    );
}

#[test]
fn runtime_boundary_may_store_executor_handle_types() {
    let directory = tempfile::tempdir().expect("a temporary source directory");
    let source = directory.path().join("runtime.rs");
    std::fs::write(&source, "struct RuntimeHandle(tokio::runtime::Handle);")
        .expect("the source probe is writable");

    assert!(
        inspect_source_with_runtime_adapters(&source, true)
            .executor_spawns
            .is_empty()
    );
}

#[test]
fn runtime_boundary_rejects_handle_spawns_outside_adapter_impls() {
    let directory = tempfile::tempdir().expect("a temporary source directory");
    let source = directory.path().join("runtime.rs");
    std::fs::write(
        &source,
        "impl Helper { fn handle() -> tokio::runtime::Handle { todo!() } } \
         struct Helper; fn bypass(make: fn() -> tokio::runtime::Handle) { \
         Helper::handle().spawn(async {}); make().spawn(async {}); }",
    )
    .expect("the source probe is writable");

    assert!(
        inspect_source_with_runtime_adapters(&source, true)
            .executor_spawns
            .iter()
            .filter(|finding| *finding == "executor handle::spawn")
            .count()
            >= 2
    );
}

fn inspect_runtime_adapter_probe(probe: &str) -> BoundaryVisitor {
    let directory = tempfile::tempdir().expect("a temporary source directory");
    let source = directory.path().join("runtime.rs");
    std::fs::write(
        &source,
        [
            "trait Runtime {} struct TokioRuntime; \
             impl Runtime for TokioRuntime { fn spawn() { tokio::spawn(async {}); } } ",
            probe,
        ]
        .concat(),
    )
    .expect("the source probe is writable");
    inspect_source_with_runtime_adapters(&source, true)
}

#[test]
fn runtime_boundary_rejects_handle_returning_trait_method_bypass() {
    assert_eq!(
        inspect_runtime_adapter_probe(
            "trait HandleProvider { fn handle(&self) -> tokio::runtime::Handle; } \
             fn bypass(provider: &impl HandleProvider) { provider.handle().spawn(async {}); }",
        )
        .executor_spawns,
        ["executor handle::spawn"]
    );
}

#[test]
fn runtime_boundary_rejects_local_handle_closure_bypass() {
    assert_eq!(
        inspect_runtime_adapter_probe(
            "fn bypass() { let make = || -> tokio::runtime::Handle { \
             tokio::runtime::Handle::current() }; \
             make().spawn(async {}); }",
        )
        .executor_spawns,
        ["executor handle::spawn"]
    );
}

#[test]
fn structural_guard_inspects_macro_token_bodies() {
    let directory = tempfile::tempdir().expect("a temporary source directory");
    let source = directory.path().join("macros.rs");
    std::fs::write(
        &source,
        "struct JsBacked; \
         macro_rules! launch { ($method:ident) => { tokio::$method(async {}) } } \
         macro_rules! fake_send { ($marker:ident) => { unsafe impl $marker for JsBacked {} } }",
    )
    .expect("the source probe is writable");

    let findings = inspect_source(&source);
    assert!(
        findings
            .executor_spawns
            .iter()
            .any(|finding| finding == "macro tokio::parameterized spawn")
    );
    assert!(
        findings
            .unsafe_send_sync_impls
            .iter()
            .any(|finding| finding == "macro parameterized Send/Sync")
    );
}

#[test]
fn source_discovery_is_recursive() {
    let directory = tempfile::tempdir().expect("a temporary source directory");
    let nested = directory.path().join("one/two");
    std::fs::create_dir_all(&nested).expect("the nested source directory is writable");
    let source = nested.join("deep.rs");
    std::fs::write(&source, "fn deep() {}").expect("the nested source probe is writable");

    assert_eq!(rust_sources(directory.path()), [source]);
}

#[test]
fn the_framework_never_fakes_send_or_sync() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for path in rust_sources(&source_root) {
        let relative = path
            .strip_prefix(&source_root)
            .expect("a source path is below the source root");
        let findings = inspect_source(&path);
        assert!(
            findings.unsafe_send_sync_impls.is_empty(),
            "{} fakes thread safety with unsafe impl(s) of {}",
            relative.display(),
            findings.unsafe_send_sync_impls.join(", ")
        );
    }
}

#[test]
fn unsafe_send_sync_guard_uses_rust_syntax() {
    let directory = tempfile::tempdir().expect("a temporary source directory");
    let source = directory.path().join("unsafe_impls.rs");
    std::fs::write(
        &source,
        "use core::marker::Send as JsSend; struct JsBacked<T>(T); \
         unsafe impl<T> ::core::marker::Send for JsBacked<T> {}\n\
         unsafe impl<T> Sync for JsBacked<T> {} \
         unsafe impl<T> JsSend for JsBacked<T> {}",
    )
    .expect("the source probe is writable");

    assert_eq!(
        inspect_source(&source).unsafe_send_sync_impls,
        ["core::marker::Send", "Sync", "JsSend"]
    );
}

#[test]
fn task_send_is_the_only_conditional_runtime_marker() {
    let runtime =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/runtime.rs"))
            .expect("the runtime source is readable");

    assert!(runtime.contains("pub trait TaskSend"));
    assert!(
        !runtime.contains("pub trait TaskSync"),
        "ADR 0020 assigns the complete native/WASM task-bound difference to TaskSend"
    );
}

#[test]
fn native_socket_entry_points_are_omitted_on_wasm() {
    let public_surface_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs");
    let public_surface = std::fs::read_to_string(&public_surface_path)
        .expect("the crate public surface is readable");
    let syntax = syn::parse_file(&public_surface).expect("the crate public surface parses as Rust");

    for (feature, export) in [("tcp", "TcpBuilder"), ("websocket", "WebSocketBuilder")] {
        let exports = syntax
            .items
            .iter()
            .filter(|item| {
                let syn::Item::Use(item_use) = item else {
                    return false;
                };
                use_tree_exports(&item_use.tree, export)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            exports.len(),
            1,
            "the {feature} entry point must have exactly one public export"
        );
        let syn::Item::Use(item_use) = exports[0] else {
            unreachable!("the filtered item is a use")
        };
        assert!(
            item_use
                .attrs
                .iter()
                .any(|attribute| cfg_matches_native_feature(attribute, feature)),
            "the {feature} entry point must stay omitted on wasm32",
        );
    }
}
