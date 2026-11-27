use super::arguments::{ArgumentMismatch, CallArguments};
use super::tuples::type_parts;
use super::*;

#[derive(Clone)]
pub(super) struct GenericRecord {
    pub name: Name,
    pub parameters: Vec<Name>,
    pub fields: Vec<(Name, Name)>,
}
#[derive(Clone)]
pub(super) struct GenericFunction {
    pub scope: scopes::ScopeId,
    pub name: Name,
    pub parameters: Vec<Name>,
    pub arguments: Vec<(DeclaredName, Name)>,
    pub result: Option<Name>,
    pub body: Option<Box<Stmt>>,
    pub foreign: Option<BTreeMap<String, String>>,
    pub origin: Origin,
    pub callbacks: Vec<CallbackRequirement>,
    pub calls: Vec<CallRequirement>,
}

#[derive(Clone)]
pub(super) struct CallbackRequirement {
    pub ty: String,
    pub parameters: Vec<String>,
    pub result: String,
}

#[derive(Clone)]
pub(super) struct CallRequirement {
    pub name: String,
    pub arguments: Vec<String>,
    pub result: String,
}

fn bound(pattern: &str, parameters: &[Name], bindings: &BTreeMap<String, String>) -> bool {
    !parameters.iter().any(|p| {
        !bindings.contains_key(&p.text)
            && pattern
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .any(|word| word == p.text)
    })
}

// Candidate selection is read-only; registration and native expansion happen
// only after a unique template and complete bindings have been selected.
struct SelectedGeneric {
    template: usize,
    bindings: BTreeMap<String, String>,
}

/// Signature-only inference and actual calls must use the same ranking.
fn best_candidates<T>(candidates: Vec<(usize, T)>) -> Vec<T> {
    let best = candidates.iter().map(|(rank, _)| *rank).min();
    candidates
        .into_iter()
        .filter(|(rank, _)| Some(*rank) == best)
        .map(|(_, candidate)| candidate)
        .collect()
}

struct QueryBudget {
    remaining: usize,
}
impl QueryBudget {
    fn new() -> Self {
        Self { remaining: 4096 }
    }
    fn charge(&mut self, depth: usize) -> Result<(), ()> {
        if depth > 32 || self.remaining == 0 {
            return Err(());
        }
        self.remaining -= 1;
        Ok(())
    }
}

impl GenericFunction {
    /// Count argument positions, not distinct type parameters or occurrences.
    /// Explicit type arguments do not change a declaration's rank.
    fn generic_argument_count(&self) -> usize {
        self.arguments
            .iter()
            .filter(|(_, ty)| {
                ty.text
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .any(|word| self.parameters.iter().any(|p| p.text == word))
            })
            .count()
    }

    fn infer_requirements(
        &self,
        compiler: &Compiler,
        depth: usize,
        budget: &mut QueryBudget,
        expected: Option<&str>,
        bindings: &mut BTreeMap<String, String>,
    ) -> Result<bool, ()> {
        budget.charge(depth)?;
        // A callable's nominal type is already known from the argument. Its
        // signature can fill result/argument parameters without erasing captures.
        let mut use_family_hints = false;
        loop {
            let before = bindings.len();
            // An operation on the source can identify an element type more
            // precisely than the callback signature that accepts the element.
            for requirement in &self.calls {
                budget.charge(depth)?;
                if bound(&requirement.result, &self.parameters, bindings)
                    || !bound(&requirement.name, &self.parameters, bindings)
                    || !requirement
                        .arguments
                        .iter()
                        .all(|a| bound(a, &self.parameters, bindings))
                {
                    continue;
                }
                let name = substitute(&requirement.name, bindings);
                let arguments = requirement
                    .arguments
                    .iter()
                    .map(|a| substitute(a, bindings))
                    .collect::<Vec<_>>();
                if let Some(result) =
                    compiler.signature_result(&name, &arguments, self.scope, depth + 1, budget)?
                    && !unify(&requirement.result, &result, &self.parameters, bindings)
                {
                    return Ok(false);
                }
            }
            for requirement in &self.callbacks {
                let ty = substitute(&requirement.ty, bindings);
                if self.parameters.iter().any(|p| {
                    !bindings.contains_key(&p.text)
                        && ty
                            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                            .any(|word| word == p.text)
                }) {
                    continue;
                }
                let Some(callback) = compiler.callables.get(&ty) else {
                    return Ok(false);
                };
                let (parameters, result) = if let Some(family) = compiler.families.get(&ty) {
                    let parameters = if requirement
                        .parameters
                        .iter()
                        .all(|p| bound(p, &self.parameters, bindings))
                    {
                        requirement
                            .parameters
                            .iter()
                            .map(|p| substitute(p, bindings))
                            .collect::<Vec<_>>()
                    } else if family.generic.is_empty() && use_family_hints {
                        use_family_hints = false;
                        let mut signatures = family
                            .concrete
                            .iter()
                            .map(|&id| compiler.functions[id].params.clone())
                            .collect::<BTreeSet<_>>();
                        if signatures.len() != 1 {
                            continue;
                        }
                        signatures.pop_first().unwrap()
                    } else {
                        continue;
                    };
                    let result_context = bound(&requirement.result, &self.parameters, bindings)
                        .then(|| substitute(&requirement.result, bindings));
                    let Some(result) = compiler.signature_result_with_context(
                        &family.name.text,
                        &parameters,
                        family.origin,
                        result_context.as_deref(),
                        depth + 1,
                        budget,
                    )?
                    else {
                        if result_context.is_none() && expected.is_some() {
                            continue;
                        }
                        return Ok(false);
                    };
                    (parameters, result)
                } else {
                    (callback.parameters.clone(), callback.result.clone())
                };
                if parameters.len() != requirement.parameters.len()
                    || !requirement
                        .parameters
                        .iter()
                        .zip(&parameters)
                        .all(|(pattern, actual)| unify(pattern, actual, &self.parameters, bindings))
                    || !unify(&requirement.result, &result, &self.parameters, bindings)
                {
                    return Ok(false);
                }
            }
            if !self.infer_result(expected, bindings) {
                return Ok(false);
            }
            if bindings.len() == before {
                if !use_family_hints {
                    use_family_hints = true;
                    continue;
                }
                return Ok(true);
            }
        }
    }
    fn infer_result(
        &self,
        expected: Option<&str>,
        bindings: &mut BTreeMap<String, String>,
    ) -> bool {
        // Context fills missing type arguments; it must not choose between
        // otherwise applicable overloads by their already determined results.
        if let Some(expected) = expected
            && self
                .parameters
                .iter()
                .any(|p| !bindings.contains_key(&p.text))
        {
            unify(
                self.result.as_ref().map_or("Unit", |r| r.text.as_str()),
                expected,
                &self.parameters,
                bindings,
            )
        } else {
            true
        }
    }
}

pub(super) fn substitute(text: &str, bindings: &BTreeMap<String, String>) -> String {
    let mut result = String::new();
    let mut word = String::new();
    for c in text.chars().chain(std::iter::once('\0')) {
        if c.is_ascii_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            result.push_str(bindings.get(&word).unwrap_or(&word));
            word.clear();
            if c != '\0' {
                result.push(c);
            }
        }
    }
    crate::frontend::ast::canonical_type(&result)
}
pub(super) fn unify(
    pattern: &str,
    actual: &str,
    parameters: &[Name],
    bindings: &mut BTreeMap<String, String>,
) -> bool {
    fn match_type(
        pattern: &str,
        actual: &str,
        parameters: &[Name],
        bindings: &mut BTreeMap<String, String>,
        root: bool,
    ) -> bool {
        if root && let Some(base) = access_types::inner(pattern) {
            return match_type(
                base,
                access_types::underlying(actual),
                parameters,
                bindings,
                true,
            );
        }
        if let Some(parameter) = parameters.iter().find(|p| p.text == pattern) {
            if actual != pattern {
                match &parameter.constant {
                    Some(kind) if constant_kind(actual) != Some(kind.as_str()) => return false,
                    None if constant_kind(actual).is_some() => return false,
                    _ => {}
                }
            }
            if let Some(known) = bindings.get(pattern) {
                return if root {
                    access_types::same(known, actual)
                } else {
                    known == actual
                };
            }
            bindings.insert(pattern.into(), actual.into());
            return true;
        }
        let actual = if root {
            access_types::underlying(actual)
        } else {
            actual
        };
        match (type_parts(pattern), type_parts(actual)) {
            (Some((a, pa)), Some((b, pb))) if a == b && pa.len() == pb.len() => pa
                .iter()
                .zip(pb)
                .all(|(p, a)| match_type(p, a, parameters, bindings, false)),
            _ => pattern == actual,
        }
    }
    match_type(pattern, actual, parameters, bindings, true)
}
pub(super) fn constant_kind(text: &str) -> Option<&'static str> {
    if text == "true" || text == "false" {
        Some("Bool")
    } else if text.parse::<i64>().is_ok() {
        Some("Int")
    } else {
        None
    }
}
pub(super) fn validate_bindings(
    parameters: &[Name],
    bindings: &BTreeMap<String, String>,
    span: &Span,
) -> Result<(), String> {
    for parameter in parameters {
        if let Some(value) = bindings.get(&parameter.text) {
            match &parameter.constant {
                Some(kind) if constant_kind(value) != Some(kind.as_str()) => {
                    return Err(span.error(format!(
                        "value argument '{}' requires a {} constant",
                        parameter.text, kind
                    )));
                }
                None if constant_kind(value).is_some() => {
                    return Err(span.error(format!(
                        "type argument '{}' requires a type",
                        parameter.text
                    )));
                }
                _ => {}
            }
        }
    }
    Ok(())
}
pub(super) fn parameters_valid(parameters: &[Name]) -> Result<(), String> {
    let mut names = BTreeSet::new();
    for parameter in parameters {
        if parameter
            .constant
            .as_deref()
            .is_some_and(|kind| kind != "Int" && kind != "Bool")
        {
            return Err(parameter
                .span
                .error("value parameters support only Int and Bool"));
        }
        if !names.insert(&parameter.text) {
            return Err(parameter.span.error("duplicate type parameter"));
        }
    }
    Ok(())
}
pub(super) fn explicit_bindings(
    name: &str,
    parameters: &[Name],
) -> Option<BTreeMap<String, String>> {
    if let Some((_, arguments)) = type_parts(name) {
        if arguments.len() != parameters.len() {
            return None;
        }
        let bindings = parameters
            .iter()
            .zip(arguments)
            .map(|(p, a)| (p.text.clone(), a.into()))
            .collect();
        validate_bindings(parameters, &bindings, &parameters.first()?.span).ok()?;
        Some(bindings)
    } else {
        Some(BTreeMap::new())
    }
}

// Ordinary constructors own their whole family. Patterned native declarations
// can coexist only when no concrete type can match both patterns.
pub(super) fn validate_type_declarations(sources: &[Source]) -> Result<(), String> {
    type Declaration<'a> = (&'a Name, &'a [Name], bool);
    let mut families: BTreeMap<&str, Vec<Declaration<'_>>> = BTreeMap::new();
    for statement in sources.iter().flat_map(|s| &s.statements) {
        let (name, parameters, whole_family) = match statement {
            Stmt::Type(name, _) => (name, &[][..], type_parts(&name.text).is_none()),
            Stmt::Struct(name, parameters, _) | Stmt::Enum(name, parameters, _) => {
                (name, parameters.as_slice(), true)
            }
            Stmt::GenericExternType(name, parameters, _) => (
                name,
                parameters.as_slice(),
                type_parts(&name.text).is_none(),
            ),
            _ => continue,
        };
        let base = type_parts(&name.text).map_or(name.text.as_str(), |(base, _)| base);
        if base.starts_with("__Rusk") {
            return Err(name.span.error("reserved internal type name"));
        }
        if base == "Tuple" {
            return Err(name.span.error("reserved tuple type constructor"));
        }
        externs::type_patterns::validate(name, parameters)?;
        let previous = families.entry(base).or_default();
        for (old, old_parameters, old_whole) in previous.iter() {
            if whole_family
                || *old_whole
                || externs::type_patterns::overlap(old, old_parameters, name, parameters)?
                || ((!parameters.is_empty() || !old_parameters.is_empty())
                    && type_parts(&old.text).map(|(_, a)| a.len())
                        != type_parts(&name.text).map(|(_, a)| a.len()))
            {
                return Err(name.span.error(format!(
                    "duplicate type family '{base}'; previous definition at {}:{}:{}",
                    old.span.file, old.span.line, old.span.column
                )));
            }
        }
        previous.push((name, parameters, whole_family));
    }
    Ok(())
}

impl Compiler {
    pub(super) fn register_generic_record(
        &mut self,
        definition: GenericRecord,
    ) -> Result<(), String> {
        parameters_valid(&definition.parameters)?;
        if self.types.contains_key(&definition.name.text)
            || self.generic_records.contains_key(&definition.name.text)
            || self
                .generic_extern_types
                .contains_key(&definition.name.text)
        {
            return Err(definition.name.span.error("duplicate type"));
        }
        let mut fields = BTreeSet::new();
        for (name, _) in &definition.fields {
            if !fields.insert(&name.text) {
                return Err(name.span.error("duplicate field"));
            }
        }
        self.generic_records
            .insert(definition.name.text.clone(), definition);
        Ok(())
    }
    pub(super) fn register_generic_function(
        &mut self,
        definition: GenericFunction,
    ) -> Result<(), String> {
        parameters_valid(&definition.parameters)?;
        let mut arguments = BTreeSet::new();
        for (name, _) in &definition.arguments {
            if definition
                .parameters
                .iter()
                .any(|p| p.constant.is_some() && p.text == name.text)
            {
                return Err(name
                    .span
                    .error("runtime parameter conflicts with value parameter"));
            }
            if !arguments.insert(&name.text) {
                return Err(name.span.error("duplicate parameter"));
            }
        }
        if definition.scope != scopes::ScopeId::ROOT && definition.body.is_some() {
            self.register_generic_environment(self.generic_functions.len());
        }
        self.generic_functions.push(definition);
        Ok(())
    }
    pub(super) fn specialize_record(&mut self, name: &Name) -> Result<bool, String> {
        let Some((base, arguments)) = type_parts(&name.text) else {
            return Ok(false);
        };
        let Some(template) = self.generic_records.get(base).cloned() else {
            return Ok(false);
        };
        if arguments.len() != template.parameters.len() {
            return Err(name.span.error(format!(
                "type '{base}' expects {} type arguments",
                template.parameters.len()
            )));
        }
        if self.records.len() >= 256 {
            return Err(name.span.error("struct specialization limit exceeded"));
        }
        let bindings: BTreeMap<String, String> = template
            .parameters
            .iter()
            .zip(arguments)
            .map(|(p, a)| (p.text.clone(), a.into()))
            .collect();
        validate_bindings(&template.parameters, &bindings, &name.span)?;
        for parameter in &template.parameters {
            if parameter.constant.is_some() {
                continue;
            }
            let ty = &bindings[&parameter.text];
            self.check_type(&Name {
                constant: None,
                text: ty.clone(),
                span: name.span.clone(),
            })?;
        }
        let fields = template
            .fields
            .iter()
            .map(|(field, ty)| {
                (
                    field.clone(),
                    Name {
                        constant: None,
                        text: substitute(&ty.text, &bindings),
                        span: ty.span.clone(),
                    },
                )
            })
            .collect();
        self.types.insert(
            name.text.clone(),
            format!("RuskRecord{}", self.records.len()),
        );
        self.records.insert(
            name.text.clone(),
            records::Record {
                name: name.clone(),
                fields,
            },
        );
        self.prepare_record(&name.text).map_err(|error| {
            format!(
                "{error}\nwhile specializing '{}' at {}:{}:{}",
                name.text, name.span.file, name.span.line, name.span.column
            )
        })?;
        Ok(true)
    }
    // Resolve only signatures: inference must not emit code, instantiate bodies,
    // or leave candidate-specific types/functions behind on a failed overload.
    fn signature_result(
        &self,
        name: &str,
        arguments: &[String],
        origin: scopes::ScopeId,
        depth: usize,
        budget: &mut QueryBudget,
    ) -> Result<Option<String>, ()> {
        self.signature_result_with_context(name, arguments, origin, None, depth, budget)
    }
    fn signature_result_with_context(
        &self,
        name: &str,
        arguments: &[String],
        origin: scopes::ScopeId,
        expected: Option<&str>,
        depth: usize,
        budget: &mut QueryBudget,
    ) -> Result<Option<String>, ()> {
        budget.charge(depth)?;
        let base = type_parts(name).map_or(name, |(base, _)| base);
        let scopes = self.function_scopes_at(name, arguments, origin);
        let lookup_name = self.source_function_name(name);
        let lookup_base = self.source_function_name(base);
        let mut results = vec![];
        if base == name {
            results.extend(
                self.functions
                    .iter()
                    .enumerate()
                    .filter(|(i, f)| {
                        !self.generic_calls.contains_key(i)
                            && !self.keyword_parameters.contains_key(i)
                            && self.aggregate_candidate(*i, arguments)
                            && scopes.contains(&f.scope)
                            && f.name.text == lookup_name
                            && f.params.len() == arguments.len()
                            && f.params
                                .iter()
                                .zip(arguments)
                                .all(|(p, a)| access_types::same(p, a))
                    })
                    .map(|(_, f)| (0, f.result.clone())),
            );
        }
        for f in &self.generic_functions {
            if !scopes.contains(&f.scope)
                || f.name.text != lookup_base
                || f.arguments.len() != arguments.len()
                || f.arguments.iter().any(|(name, _)| name.keyword.is_some())
            {
                continue;
            }
            let Some(mut bindings) = explicit_bindings(name, &f.parameters) else {
                continue;
            };
            if f.arguments
                .iter()
                .zip(arguments)
                .all(|((_, pattern), ty)| unify(&pattern.text, ty, &f.parameters, &mut bindings))
                && f.infer_requirements(self, depth + 1, budget, expected, &mut bindings)?
                && f.parameters.iter().all(|p| bindings.contains_key(&p.text))
            {
                results.push((
                    f.generic_argument_count(),
                    f.result
                        .as_ref()
                        .map_or("Unit".into(), |r| substitute(&r.text, &bindings)),
                ));
            }
        }
        let results = best_candidates(results);
        if results.is_empty()
            && super::ordering::fallback(name).is_some()
            && self
                .signature_result("__cmp__", arguments, origin, depth + 1, budget)?
                .as_deref()
                == Some("Ordering")
        {
            return Ok(Some("Bool".into()));
        }
        if results.is_empty()
            && name == "__cmp__"
            && let [left, right] = arguments
            && left == right
            && let Some(("Tuple", fields)) = type_parts(left)
        {
            for field in fields {
                if self
                    .signature_result(
                        "__cmp__",
                        &[field.into(), field.into()],
                        origin,
                        depth + 1,
                        budget,
                    )?
                    .as_deref()
                    != Some("Ordering")
                {
                    return Ok(None);
                }
            }
            return Ok(Some("Ordering".into()));
        }
        Ok((results.len() == 1).then(|| results[0].clone()))
    }

    // A contextual receiver type must also be compatible with the expression
    // that constructs it. Inspect declared signatures without evaluating code.
    pub(super) fn expression_accepts_hint(&self, expression: &Expr, expected: &str) -> bool {
        let (name, shape) = match &expression.kind {
            ExprKind::Call(name, args) => {
                let Ok(shape) = CallArguments::new(0, args) else {
                    return false;
                };
                (name.text.as_str(), shape)
            }
            ExprKind::Var(name) if self.local_variable(&name.text).is_none() => {
                (name.text.as_str(), CallArguments::positional(0))
            }
            _ => return true,
        };
        if let Some((owner, variant)) = name.rsplit_once("::") {
            let (base, args) = type_parts(owner).unwrap_or((owner, vec![]));
            if let Some(definition) = self.enum_definitions.get(base)
                && definition.variants.iter().any(|(v, fields)| {
                    v.text == variant && !shape.has_keywords() && fields.len() == shape.len()
                })
            {
                let pattern = if definition.parameters.is_empty() {
                    base.to_string()
                } else {
                    format!(
                        "{base}<{}>",
                        definition
                            .parameters
                            .iter()
                            .map(|p| p.text.as_str())
                            .collect::<Vec<_>>()
                            .join(",")
                    )
                };
                let mut bindings = BTreeMap::new();
                if !args.is_empty() {
                    if args.len() != definition.parameters.len() {
                        return false;
                    }
                    bindings.extend(
                        definition
                            .parameters
                            .iter()
                            .zip(args)
                            .map(|(p, t)| (p.text.clone(), t.to_string())),
                    );
                }
                return unify(&pattern, expected, &definition.parameters, &mut bindings);
            }
        }
        let base = type_parts(name).map_or(name, |(base, _)| base);
        let scopes = self.function_scopes(name, &[]);
        let lookup_name = self.source_function_name(name);
        let lookup_base = self.source_function_name(base);
        let mut found = false;
        for (id, function) in self.functions.iter().enumerate() {
            if function.name.text == lookup_name
                && scopes.contains(&function.scope)
                && shape
                    .slots(
                        function.params.len(),
                        self.keyword_parameters.get(&id).into_iter().flatten(),
                    )
                    .is_ok()
                && !self.generic_calls.contains_key(&id)
            {
                found = true;
                if access_types::same(&function.result, expected) {
                    return true;
                }
            }
        }
        for function in &self.generic_functions {
            if !scopes.contains(&function.scope)
                || function.name.text != lookup_base
                || shape
                    .slots(
                        function.arguments.len(),
                        function.arguments.iter().map(|(name, _)| name),
                    )
                    .is_err()
            {
                continue;
            }
            found = true;
            let Some(mut bindings) = explicit_bindings(name, &function.parameters) else {
                continue;
            };
            if unify(
                function.result.as_ref().map_or("Unit", |r| r.text.as_str()),
                expected,
                &function.parameters,
                &mut bindings,
            ) {
                return true;
            }
        }
        !found
    }

    pub(super) fn callback_parameter_hints(
        &self,
        name: &str,
        count: usize,
        known: &[Value],
        expected: Option<&str>,
    ) -> BTreeSet<Vec<String>> {
        let base = type_parts(name).map_or(name, |(base, _)| base);
        let lookup = self.source_function_name(base);
        let scopes = self.function_scopes(
            name,
            &known
                .iter()
                .map(|value| value.ty.clone())
                .collect::<Vec<_>>(),
        );
        let mut hints = BTreeSet::new();
        for (template, function) in self.generic_functions.iter().enumerate() {
            if function.name.text != lookup
                || !scopes.contains(&function.scope)
                || function.arguments.len() != count
            {
                continue;
            }
            let Some(mut bindings) = explicit_bindings(name, &function.parameters) else {
                continue;
            };
            if !function
                .arguments
                .iter()
                .zip(known)
                .all(|((_, ty), value)| {
                    unify(&ty.text, &value.ty, &function.parameters, &mut bindings)
                })
            {
                continue;
            }
            if !matches!(
                function.infer_requirements(
                    self,
                    0,
                    &mut QueryBudget::new(),
                    expected,
                    &mut bindings
                ),
                Ok(true)
            ) {
                continue;
            }
            let callback_type = &function.arguments[known.len()].1.text;
            if function.body.is_some()
                && known.len() + 1 == count
                && self.lambda_probe.is_none()
                && function.parameters.iter().any(|p| p.text == *callback_type)
                && !bindings.contains_key(callback_type)
                && function
                    .parameters
                    .iter()
                    .all(|p| p.text == *callback_type || bindings.contains_key(&p.text))
                && let Some(signature) =
                    self.probe_callback(template, callback_type, bindings.clone())
            {
                hints.insert(signature);
            }
            for requirement in &function.callbacks {
                if &requirement.ty == callback_type
                    && requirement
                        .parameters
                        .iter()
                        .all(|ty| bound(ty, &function.parameters, &bindings))
                {
                    hints.insert(
                        requirement
                            .parameters
                            .iter()
                            .map(|ty| substitute(ty, &bindings))
                            .collect(),
                    );
                }
            }
        }
        hints
    }

    fn probe_callback(
        &self,
        template: usize,
        callback_type: &str,
        mut bindings: BTreeMap<String, String>,
    ) -> Option<Vec<String>> {
        // The probe stops at the callback invocation. All of its declarations,
        // capture layouts and partially checked bodies belong to the clone.
        let mut probe = self.clone();
        let name = probe.generic_functions[template].name.clone();
        let placeholder = probe
            .lambda(
                &[],
                &Expr {
                    kind: ExprKind::Block(vec![], None),
                    span: name.span.clone(),
                },
                &name.span,
            )
            .ok()?;
        bindings.insert(callback_type.into(), placeholder.ty.clone());
        probe.lambda_probe = Some(callables::LambdaProbe {
            ty: placeholder.ty,
            signature: None,
        });
        let function = probe
            .instantiate_generic(
                SelectedGeneric { template, bindings },
                &name.text,
                &[],
                &name.span,
            )
            .ok()?;
        let _ = probe.compile_function(function);
        probe.lambda_probe?.signature
    }

    pub(super) fn has_keyword_candidates(&self, name: &str) -> bool {
        let lookup = self.source_function_name(name);
        let base = type_parts(&lookup).map_or(lookup.as_ref(), |(base, _)| base);
        self.keyword_parameters.keys().any(|id| {
            !self.generic_calls.contains_key(id) && self.functions[*id].name.text == lookup
        }) || self.generic_functions.iter().any(|f| {
            f.name.text == base && f.arguments.iter().any(|(name, _)| name.keyword.is_some())
        })
    }

    fn shaped_candidates<'a>(
        &'a self,
        name: &str,
        values: &[Value],
    ) -> (
        impl Iterator<Item = (usize, &'a Function)>,
        impl Iterator<Item = (usize, &'a GenericFunction)>,
    ) {
        let arguments: Vec<_> = values.iter().map(|value| value.ty.clone()).collect();
        let scopes = self.function_scopes(name, &arguments);
        let concrete_scopes = scopes.clone();
        let lookup = self.source_function_name(name).into_owned();
        let base = type_parts(&lookup)
            .map_or(lookup.as_str(), |(base, _)| base)
            .to_string();
        let explicit = type_parts(name).is_some();
        let concrete = self.functions.iter().enumerate().filter(move |(id, f)| {
            !explicit
                && !self.generic_calls.contains_key(id)
                && self.aggregate_candidate(*id, &arguments)
                && concrete_scopes.contains(&f.scope)
                && f.name.text == lookup
        });
        let generic = self
            .generic_functions
            .iter()
            .enumerate()
            .filter(move |(_, f)| scopes.contains(&f.scope) && f.name.text == base);
        (concrete, generic)
    }

    fn shaped_generic_bindings(
        &self,
        function: &GenericFunction,
        name: &str,
        slots: &[usize],
        values: &[Value],
        expected: Option<&str>,
    ) -> Result<Option<BTreeMap<String, String>>, ()> {
        let Some(mut bindings) = explicit_bindings(name, &function.parameters) else {
            return Ok(None);
        };
        let base = type_parts(name).map_or(name, |(base, _)| base);
        if return_selected(base)
            && let Some(result) = expected
            && !unify(
                function
                    .result
                    .as_ref()
                    .map_or("Unit", |ty| ty.text.as_str()),
                result,
                &function.parameters,
                &mut bindings,
            )
        {
            return Ok(None);
        }
        if !slots.iter().zip(values).all(|(&slot, value)| {
            unify(
                &function.arguments[slot].1.text,
                &value.ty,
                &function.parameters,
                &mut bindings,
            )
        }) || !function.infer_requirements(
            self,
            0,
            &mut QueryBudget::new(),
            expected,
            &mut bindings,
        )? {
            return Ok(None);
        }
        Ok(Some(bindings))
    }

    pub(super) fn shaped_argument_hints(
        &self,
        name: &str,
        shape: &CallArguments<'_>,
        values: &[Value],
        expected: Option<&str>,
        needs_callback: bool,
    ) -> (BTreeSet<String>, BTreeSet<Vec<String>>) {
        let (concrete, generic) = self.shaped_candidates(name, values);
        let mut types = BTreeSet::new();
        let mut callbacks = BTreeSet::new();
        for (id, function) in concrete {
            let names = self.keyword_parameters.get(&id).into_iter().flatten();
            if let Ok(slots) = shape.slots(function.params.len(), names)
                && slots
                    .iter()
                    .zip(values)
                    .all(|(&slot, value)| access_types::same(&function.params[slot], &value.ty))
            {
                types.insert(function.params[slots[values.len()]].clone());
            }
        }
        for (id, function) in generic {
            let Ok(slots) = shape.slots(
                function.arguments.len(),
                function.arguments.iter().map(|(name, _)| name),
            ) else {
                continue;
            };
            let Ok(Some(bindings)) =
                self.shaped_generic_bindings(function, name, &slots, values, expected)
            else {
                continue;
            };
            let ty = &function.arguments[slots[values.len()]].1.text;
            if bound(ty, &function.parameters, &bindings) {
                types.insert(substitute(ty, &bindings));
            }
            if !needs_callback {
                continue;
            }
            for requirement in &function.callbacks {
                if &requirement.ty == ty
                    && requirement
                        .parameters
                        .iter()
                        .all(|ty| bound(ty, &function.parameters, &bindings))
                {
                    callbacks.insert(
                        requirement
                            .parameters
                            .iter()
                            .map(|ty| substitute(ty, &bindings))
                            .collect(),
                    );
                }
            }
            if function.body.is_some()
                && self.lambda_probe.is_none()
                && function
                    .parameters
                    .iter()
                    .any(|parameter| parameter.text == *ty)
                && !bindings.contains_key(ty)
                && function.parameters.iter().all(|parameter| {
                    parameter.text == *ty || bindings.contains_key(&parameter.text)
                })
                && let Some(signature) = self.probe_callback(id, ty, bindings)
            {
                callbacks.insert(signature);
            }
        }
        (types, callbacks)
    }

    pub(super) fn resolve_shaped_call(
        &mut self,
        name: &str,
        shape: &CallArguments<'_>,
        values: &[Value],
        expected: Option<&str>,
        span: &Span,
    ) -> Result<Option<(usize, Vec<usize>)>, String> {
        enum Target {
            Concrete(usize),
            Generic(SelectedGeneric),
        }
        let mut candidates = Vec::new();
        let mut errors = Vec::new();
        let (concrete, generic) = self.shaped_candidates(name, values);
        let mut matching_shape = false;
        for (id, f) in concrete {
            let names = self.keyword_parameters.get(&id).into_iter().flatten();
            let slots = match shape.slots(f.params.len(), names) {
                Ok(slots) => slots,
                Err(error) => {
                    errors.push(error);
                    continue;
                }
            };
            matching_shape = true;
            if slots
                .iter()
                .zip(values)
                .all(|(&slot, value)| access_types::same(&f.params[slot], &value.ty))
                && (!return_selected(name)
                    || expected.is_none_or(|ty| access_types::same(&f.result, ty)))
            {
                candidates.push((0, (Target::Concrete(id), slots)));
            }
        }
        for (id, f) in generic {
            let slots =
                match shape.slots(f.arguments.len(), f.arguments.iter().map(|(name, _)| name)) {
                    Ok(slots) => slots,
                    Err(error) => {
                        errors.push(error);
                        continue;
                    }
                };
            matching_shape = true;
            if let Some(bindings) = self
                .shaped_generic_bindings(f, name, &slots, values, expected)
                .map_err(|()| span.error("native reference inference limit exceeded"))?
                && f.parameters.iter().all(|p| bindings.contains_key(&p.text))
            {
                candidates.push((
                    f.generic_argument_count(),
                    (
                        Target::Generic(SelectedGeneric {
                            template: id,
                            bindings,
                        }),
                        slots,
                    ),
                ));
            }
        }
        let mut candidates = best_candidates(candidates);
        if candidates.len() > 1 {
            return Err(ambiguous_call(
                name,
                span,
                false,
                candidates.iter().map(|(target, _)| match target {
                    Target::Concrete(id) => &self.functions[*id].name,
                    Target::Generic(selected) => &self.generic_functions[selected.template].name,
                }),
            ));
        }
        if let Some((target, slots)) = candidates.pop() {
            let id = match target {
                Target::Concrete(id) => id,
                Target::Generic(selected) => {
                    self.instantiate_generic(selected, name, values, span)?
                }
            };
            return Ok(Some((id, slots)));
        }
        if !matching_shape {
            if let Some(error) = errors
                .iter()
                .find(|error| matches!(error, ArgumentMismatch::Missing(_)))
            {
                return Err(error.diagnostic(span));
            }
            if shape.has_keywords()
                && let Some(error) = errors.first()
            {
                let error = errors
                    .iter()
                    .find(|error| matches!(error, ArgumentMismatch::PositionalCount))
                    .unwrap_or(error);
                return Err(error.diagnostic(span));
            }
        }
        Ok(None)
    }

    pub(super) fn argument_hints(
        &self,
        name: &str,
        count: usize,
        known: &[Value],
        expected: Option<&str>,
    ) -> BTreeSet<String> {
        let base = type_parts(name).map_or(name, |(base, _)| base);
        let scopes = self.function_scopes(
            name,
            &known.iter().map(|v| v.ty.clone()).collect::<Vec<_>>(),
        );
        let mut hints = BTreeSet::new();
        let lookup_name = self.source_function_name(name);
        let lookup_base = self.source_function_name(base);
        if base == name {
            for (index, f) in self.functions.iter().enumerate() {
                if self.generic_instances.values().any(|&i| i == index) {
                    continue;
                }
                if f.name.text == lookup_name
                    && scopes.contains(&f.scope)
                    && f.params.len() == count
                    && f.params
                        .iter()
                        .zip(known)
                        .all(|(ty, value)| access_types::same(ty, &value.ty))
                {
                    hints.insert(f.params[known.len()].clone());
                }
            }
        }
        for f in &self.generic_functions {
            if !scopes.contains(&f.scope)
                || f.name.text != lookup_base
                || f.arguments.len() != count
            {
                continue;
            }
            let Some(mut bindings) = explicit_bindings(name, &f.parameters) else {
                continue;
            };
            if !f
                .arguments
                .iter()
                .zip(known)
                .all(|((_, ty), value)| unify(&ty.text, &value.ty, &f.parameters, &mut bindings))
            {
                continue;
            }
            let Ok(inferred) =
                f.infer_requirements(self, 0, &mut QueryBudget::new(), expected, &mut bindings)
            else {
                return BTreeSet::new();
            };
            if !inferred {
                continue;
            }
            let ty = &f.arguments[known.len()].1.text;
            let unbound = f.parameters.iter().any(|p| {
                !bindings.contains_key(&p.text)
                    && ty
                        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                        .any(|s| s == p.text)
            });
            if !unbound {
                hints.insert(substitute(ty, &bindings));
            }
        }
        hints
    }
    pub(super) fn resolve(
        &mut self,
        name: &str,
        args: &[Value],
        expected: Option<&str>,
        span: &Span,
    ) -> Result<Option<usize>, String> {
        let base = type_parts(name).map_or(name, |(base, _)| base);
        let concrete = if base == name {
            self.select(name, args, expected, span)?
        } else {
            None
        };
        if base != name
            && let Some((_, arguments)) = type_parts(name)
        {
            for ty in arguments {
                if constant_kind(ty).is_some() {
                    continue;
                }
                self.check_type(&Name {
                    constant: None,
                    text: ty.into(),
                    span: span.clone(),
                })?;
            }
        }
        let selection =
            self.select_generic_candidate(name, base, args, expected, span, concrete.is_some())?;
        if let Some(index) = concrete {
            if let Some(selection) = selection {
                return Err(ambiguous_call(
                    name,
                    span,
                    false,
                    [
                        &self.functions[index].name,
                        &self.generic_functions[selection.template].name,
                    ],
                ));
            }
            return Ok(Some(index));
        }
        let Some(selection) = selection else {
            return Ok(None);
        };
        self.instantiate_generic(selection, name, args, span)
            .map(Some)
    }
    fn select_generic_candidate(
        &self,
        name: &str,
        base: &str,
        args: &[Value],
        expected: Option<&str>,
        span: &Span,
        concrete: bool,
    ) -> Result<Option<SelectedGeneric>, String> {
        let mut candidates = vec![];
        let lookup_base = self.source_function_name(base);
        let scopes =
            self.function_scopes(name, &args.iter().map(|v| v.ty.clone()).collect::<Vec<_>>());
        for (index, f) in self.generic_functions.iter().enumerate() {
            if !scopes.contains(&f.scope)
                || f.name.text != lookup_base
                || f.arguments.len() != args.len()
                || f.arguments.iter().any(|(name, _)| name.keyword.is_some())
            {
                continue;
            }
            if concrete && f.generic_argument_count() != 0 {
                continue;
            }
            let Some(mut bindings) = explicit_bindings(name, &f.parameters) else {
                continue;
            };
            if return_selected(base)
                && let Some(expected) = expected
                && !unify(
                    f.result.as_ref().map_or("Unit", |r| r.text.as_str()),
                    expected,
                    &f.parameters,
                    &mut bindings,
                )
            {
                continue;
            }
            if f.arguments
                .iter()
                .zip(args)
                .all(|((_, ty), value)| unify(&ty.text, &value.ty, &f.parameters, &mut bindings))
                && f.infer_requirements(self, 0, &mut QueryBudget::new(), expected, &mut bindings)
                    .map_err(|()| span.error("native reference inference limit exceeded"))?
                && f.parameters.iter().all(|p| bindings.contains_key(&p.text))
            {
                candidates.push((f.generic_argument_count(), (index, bindings)));
            }
        }
        let mut candidates = best_candidates(candidates);
        if candidates.len() > 1 {
            return Err(ambiguous_call(
                name,
                span,
                true,
                candidates
                    .iter()
                    .map(|(index, _)| &self.generic_functions[*index].name),
            ));
        }
        Ok(candidates
            .pop()
            .map(|(template, bindings)| SelectedGeneric { template, bindings }))
    }
    fn instantiate_generic(
        &mut self,
        selection: SelectedGeneric,
        name: &str,
        _args: &[Value],
        span: &Span,
    ) -> Result<usize, String> {
        let SelectedGeneric { template, bindings } = selection;
        let definition = self.generic_functions[template].clone();
        validate_bindings(&definition.parameters, &bindings, span)?;
        let type_arguments = definition
            .parameters
            .iter()
            .map(|p| bindings[&p.text].clone())
            .collect::<Vec<_>>();
        let key = (template, type_arguments);
        if let Some(index) = self.generic_instances.get(&key) {
            return Ok(*index);
        }
        if self.generic_instances.len() >= 256 {
            return Err(span.error("generic function specialization limit exceeded"));
        }
        let result = definition
            .result
            .as_ref()
            .map_or("Unit".into(), |ty| substitute(&ty.text, &bindings));
        self.check_type(&Name {
            constant: None,
            text: result.clone(),
            span: span.clone(),
        })?;
        let mut body = definition.body.clone();
        if let Some(body) = &mut body {
            substitute_statement(body, &bindings);
            if let Stmt::Block(statements, _) = body.as_mut() {
                for parameter in definition
                    .parameters
                    .iter()
                    .rev()
                    .filter(|p| p.constant.is_some())
                {
                    let mut declared = DeclaredName::plain(parameter.clone());
                    declared.binding = Some(crate::frontend::ast::BindingAnnotation {
                        mode: BindingMode::Fixed,
                        span: parameter.span.clone(),
                    });
                    declared.access = Some(crate::frontend::ast::AccessAnnotation {
                        mode: crate::frontend::ast::AccessMode::Readonly,
                        span: parameter.span.clone(),
                    });
                    statements.insert(
                        0,
                        Stmt::Let(
                            Binding::Name(declared),
                            None,
                            Some(Initializer::Shared(Expr {
                                span: parameter.span.clone(),
                                kind: ExprKind::Literal(
                                    parameter.constant.clone().unwrap(),
                                    bindings[&parameter.text].clone(),
                                ),
                            })),
                        ),
                    );
                }
            }
        }
        let index = self.functions.len();
        let cpp = format!("rusk_generic_{index}");
        self.functions.push(Function {
            scope: definition.scope,
            name: definition.name.clone(),
            params: definition
                .arguments
                .iter()
                .map(|(_, ty)| substitute(&ty.text, &bindings))
                .collect(),
            result,
            cpp: cpp.clone(),
            code: String::new(),
            body: body.map(|body| {
                (
                    definition
                        .arguments
                        .iter()
                        .map(|(n, _)| n.clone())
                        .collect(),
                    body,
                )
            }),
            origin: definition.origin,
            checked: None,
        });
        if definition
            .arguments
            .iter()
            .any(|(name, _)| name.keyword.is_some())
        {
            self.keyword_parameters.insert(
                index,
                definition
                    .arguments
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect(),
            );
        }
        self.generic_instances.insert(key, index);
        self.generic_calls.insert(index, span.clone());
        self.register_generic_instance_environment(index, template);
        let parent_scope = self.context.declaration_scope;
        self.context.declaration_scope = definition.scope;
        let expanded = self.expand_generic_body(index, &definition, &bindings, name, span);
        self.context.declaration_scope = parent_scope;
        expanded?;
        Ok(index)
    }
    fn expand_generic_body(
        &mut self,
        index: usize,
        definition: &GenericFunction,
        bindings: &BTreeMap<String, String>,
        name: &str,
        span: &Span,
    ) -> Result<(), String> {
        let cpp = self.functions[index].cpp.clone();
        if let Some(properties) = &definition.foreign {
            self.register_return_abi(
                index,
                &definition.name,
                properties,
                &self.functions[index].result.clone(),
            )?;
        }
        let code = if let Some(properties) = &definition.foreign {
            self.render_native(
                &property(&definition.name, properties, "code")?,
                bindings,
                Some(&cpp),
                &definition.name.span,
                true,
                Some(index),
            )
            .map_err(|error| {
                format!(
                    "{error}\nwhile instantiating '{name}' at {}:{}:{}",
                    span.file, span.line, span.column
                )
            })?
        } else {
            String::new()
        };
        if let Some(properties) = &definition.foreign {
            let native = self
                .declared_native_effects(
                    &definition.name,
                    &self.functions[index].params,
                    &self.functions[index].result,
                    properties,
                    definition.origin,
                )
                .map_err(|error| {
                    format!(
                        "{error}\nwhile instantiating '{name}' at {}:{}:{}",
                        span.file, span.line, span.column
                    )
                })?;
            let author = effects::extern_parameter_contract(&definition.arguments, properties)?;
            if let Some(contract) = native.or(author) {
                self.native_info_mut(index).contract = Some(contract);
            }
            let mut properties = properties.clone();
            for key in ["lower", "lower_owned", "lower_borrowed", "lower_rooted"] {
                if let Some(expression) = properties.get(key).cloned() {
                    let expression = self.render_native(
                        &expression,
                        bindings,
                        Some(&cpp),
                        &definition.name.span,
                        true,
                        Some(index),
                    )?;
                    properties.insert(key.into(), expression);
                }
            }
            self.register_native_lowering(
                index,
                &definition.name,
                &self.functions[index].params.clone(),
                &self.functions[index].result.clone(),
                &properties,
                definition.origin,
            )
            .map_err(|error| {
                format!(
                    "{error}\nwhile instantiating '{name}' at {}:{}:{}",
                    span.file, span.line, span.column
                )
            })?;
        }
        self.functions[index].code = code;
        Ok(())
    }
}

fn substitute_name(name: &mut Name, bindings: &BTreeMap<String, String>) {
    name.text = substitute(&name.text, bindings);
}
fn substitute_reference(name: &mut Name, bindings: &BTreeMap<String, String>) {
    if let Some((owner, variant)) = name.text.rsplit_once("::") {
        name.text = format!("{}::{variant}", substitute(owner, bindings));
    } else {
        substitute_name(name, bindings);
    }
}
fn substitute_expr(expr: &mut Expr, bindings: &BTreeMap<String, String>) {
    match &mut expr.kind {
        ExprKind::Block(items, tail) => {
            let bindings = scope_type_bindings(items, bindings);
            for item in items {
                substitute_statement(item, &bindings);
            }
            if let Some(tail) = tail {
                substitute_expr(tail, &bindings);
            }
        }
        ExprKind::Match(value, arms) => {
            substitute_expr(value, bindings);
            for arm in arms {
                substitute_pattern(&mut arm.pattern, bindings);
                if let Some(guard) = &mut arm.guard {
                    substitute_expr(guard, bindings);
                }
                substitute_expr(&mut arm.body, bindings);
            }
        }
        ExprKind::If(a, b, c) => {
            substitute_condition(a, bindings);
            substitute_expr(b, bindings);
            substitute_expr(c, bindings);
        }
        ExprKind::CompareChain(first, rest) => {
            substitute_expr(first, bindings);
            for (_, value, _) in rest {
                substitute_expr(value, bindings);
            }
        }
        ExprKind::Unwrap(value) => substitute_expr(value, bindings),
        ExprKind::Coalesce(a, b) => {
            substitute_expr(a, bindings);
            substitute_expr(b, bindings);
        }
        ExprKind::Lambda(parameters, body) => {
            for (_, ty) in parameters {
                if let Some(ty) = ty {
                    substitute_name(ty, bindings);
                }
            }
            substitute_expr(body, bindings);
        }
        ExprKind::Invoke(callee, args) => {
            substitute_expr(callee, bindings);
            for arg in args {
                substitute_expr(&mut arg.value, bindings);
            }
        }
        ExprKind::Array(items) | ExprKind::Tuple(items) => {
            for item in items {
                substitute_expr(item, bindings);
            }
        }
        ExprKind::Repeat(a, b)
        | ExprKind::Range(a, b)
        | ExprKind::Index(a, b)
        | ExprKind::Logical(_, a, b)
        | ExprKind::Binary(_, a, b)
        | ExprKind::Assign(_, a, b) => {
            substitute_expr(a, bindings);
            substitute_expr(b, bindings);
        }
        ExprKind::Field(value, _) | ExprKind::Unary(_, value) => substitute_expr(value, bindings),
        ExprKind::Record(name, fields) => {
            substitute_name(name, bindings);
            for (_, value) in fields {
                substitute_expr(value, bindings);
            }
        }
        ExprKind::Call(name, args) => {
            substitute_reference(name, bindings);
            for arg in args {
                substitute_expr(&mut arg.value, bindings);
            }
        }
        ExprKind::Var(n) if n.text.contains("::") => substitute_reference(n, bindings),
        ExprKind::Literal(..) | ExprKind::Var(_) => {}
    }
}
fn unshadowed_bindings(
    parameters: &[Name],
    bindings: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    bindings
        .iter()
        .filter(|(name, _)| !parameters.iter().any(|parameter| &parameter.text == *name))
        .map(|(name, ty)| (name.clone(), ty.clone()))
        .collect()
}

fn scope_type_bindings(
    statements: &[Stmt],
    bindings: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let parameters = statements
        .iter()
        .filter_map(|statement| match statement {
            Stmt::Struct(name, ..) | Stmt::Enum(name, ..) => Some(name.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    unshadowed_bindings(&parameters, bindings)
}

fn substitute_statement(stmt: &mut Stmt, bindings: &BTreeMap<String, String>) {
    match stmt {
        Stmt::Struct(_, parameters, fields) => {
            let bindings = unshadowed_bindings(parameters, bindings);
            for (_, ty) in fields {
                substitute_name(ty, &bindings);
            }
        }
        Stmt::Enum(_, parameters, variants) => {
            let bindings = unshadowed_bindings(parameters, bindings);
            for (_, fields) in variants {
                for ty in fields {
                    substitute_name(ty, &bindings);
                }
            }
        }
        Stmt::UserFunction(_, parameters, arguments, result, body) => {
            let bindings = unshadowed_bindings(parameters, bindings);
            for (parameter, ty) in arguments {
                substitute_name(ty, &bindings);
                if let Some(ParameterDefault::Expression(default)) = parameter
                    .keyword
                    .as_mut()
                    .and_then(|keyword| keyword.default.as_mut())
                {
                    substitute_expr(default, &bindings);
                }
            }
            if let Some(ty) = result {
                substitute_name(ty, &bindings);
            }
            substitute_statement(body, &bindings);
        }
        Stmt::Let(_, ty, initializer) => {
            if let Some(ty) = ty {
                substitute_name(ty, bindings);
            }
            if let Some(Initializer::Shared(expr) | Initializer::Copied(expr)) = initializer {
                substitute_expr(expr, bindings);
            }
        }
        Stmt::Return(_, Some(value)) => substitute_expr(value, bindings),
        Stmt::Input(items) => {
            for (_, ty, args) in items {
                substitute_name(ty, bindings);
                for arg in args {
                    substitute_expr(arg, bindings);
                }
            }
        }
        Stmt::Output(items) => {
            for expr in items {
                substitute_expr(expr, bindings);
            }
        }
        Stmt::Expr(expr) | Stmt::Match(expr) => substitute_expr(expr, bindings),
        Stmt::Block(items, tail) => {
            let bindings = scope_type_bindings(items, bindings);
            for item in items {
                substitute_statement(item, &bindings);
            }
            if let Some(tail) = tail {
                substitute_expr(tail, &bindings);
            }
        }
        Stmt::Labeled(_, body) => substitute_statement(body, bindings),
        Stmt::If(condition, body, other) | Stmt::While(condition, body, other) => {
            substitute_condition(condition, bindings);
            substitute_statement(body, bindings);
            if let Some(other) = other {
                substitute_statement(other, bindings);
            }
        }
        Stmt::For(_, source, body, other) => {
            substitute_expr(source, bindings);
            substitute_statement(body, bindings);
            if let Some(other) = other {
                substitute_statement(other, bindings);
            }
        }
        Stmt::Loop(body) => substitute_statement(body, bindings),
        _ => {}
    }
}

fn substitute_pattern(p: &mut Pattern, bindings: &BTreeMap<String, String>) {
    match &mut p.kind {
        PatternKind::Named(n, children) => {
            if n.text.contains("::") {
                substitute_reference(n, bindings);
            }
            if let Some(children) = children {
                for p in children {
                    substitute_pattern(p, bindings);
                }
            }
        }
        PatternKind::Tuple(items) | PatternKind::Or(items) => {
            for p in items {
                substitute_pattern(p, bindings);
            }
        }
        PatternKind::Record(n, fields, _) => {
            substitute_name(n, bindings);
            for (_, p) in fields {
                substitute_pattern(p, bindings);
            }
        }
        PatternKind::Literal(..) => {}
    }
}
fn substitute_condition(c: &mut Condition, bindings: &BTreeMap<String, String>) {
    match c {
        Condition::Expr(e) => substitute_expr(e, bindings),
        Condition::Let(p, e) => {
            substitute_pattern(p, bindings);
            substitute_expr(e, bindings);
        }
    }
}

#[cfg(test)]
mod namespace_tests;
