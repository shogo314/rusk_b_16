//! Source argument labels, and default values evaluated in the declaration scope.
use super::*;

impl Compiler {
    pub(super) fn expand_source_defaults(&mut self, sources: &[Source]) -> Vec<Source> {
        sources
            .iter()
            .map(|source| Source {
                statements: self.expand_parameter_defaults(&source.statements),
                origin: source.origin,
            })
            .collect()
    }

    pub(super) fn expand_parameter_defaults(&mut self, statements: &[Stmt]) -> Vec<Stmt> {
        let mut expanded = Vec::with_capacity(statements.len());
        for statement in statements {
            let mut statement = statement.clone();
            let (generics, parameters) = match &mut statement {
                Stmt::UserFunction(_, generics, parameters, ..)
                | Stmt::GenericExternFunction(_, generics, parameters, ..) => {
                    (generics.as_slice(), parameters)
                }
                Stmt::Function(_, parameters, ..) => (&[][..], parameters),
                _ => {
                    expanded.push(statement);
                    continue;
                }
            };
            if !parameters.iter().any(|(name, _)| {
                name.keyword.as_ref().is_some_and(|keyword| {
                    matches!(keyword.default, Some(ParameterDefault::Expression(_)))
                })
            }) {
                expanded.push(statement);
                continue;
            }
            let mut earlier = vec![];
            for (parameter, ty) in parameters {
                let span = parameter.name.span.clone();
                if let Some(default) = parameter.keyword.as_mut().and_then(|p| p.default.as_mut())
                    && let ParameterDefault::Expression(expression) = default
                {
                    // Ordinary Rusk bodies give defaults lexical lookup, captures,
                    // parameter bindings and return checking even for externs.
                    let name = Name {
                        text: format!("$default_{}", self.default_function_counter),
                        span,
                        constant: None,
                    };
                    self.default_function_counter += 1;
                    expanded.push(Stmt::UserFunction(
                        name.clone(),
                        generics.to_vec(),
                        earlier.clone(),
                        Some(ty.clone()),
                        Box::new(Stmt::Block(
                            vec![Stmt::Return(
                                expression.span.clone(),
                                Some((**expression).clone()),
                            )],
                            None,
                        )),
                    ));
                    *default = ParameterDefault::Function(name);
                }
                let mut prefix_parameter = parameter.clone();
                prefix_parameter.keyword = None;
                // Supplying the computed value to the eventual callee is not
                // a return from that callee's noescape parameter.
                prefix_parameter.escape = None;
                earlier.push((prefix_parameter, ty.clone()));
            }
            expanded.push(statement);
        }
        expanded
    }
}

pub(super) struct CallArguments<'a> {
    positional: usize,
    keywords: Vec<&'a Name>,
}

#[derive(Clone, Copy)]
pub(super) enum ArgumentMismatch<'a> {
    PositionalCount,
    Unknown(&'a Name),
    Missing(&'a str),
}

impl ArgumentMismatch<'_> {
    pub(super) fn diagnostic(&self, span: &Span) -> String {
        match self {
            Self::PositionalCount => span.error("incorrect number of positional arguments"),
            Self::Unknown(label) => label
                .span
                .error(format!("unknown named argument '{}'", label.text)),
            Self::Missing(name) => span.error(format!("missing required named argument '{name}'")),
        }
    }
}

impl<'a> CallArguments<'a> {
    pub(super) fn new(prefix: usize, source: &'a [Argument]) -> Result<Self, String> {
        let mut positional = prefix;
        let mut keywords: Vec<&Name> = vec![];
        for arg in source {
            if let Some(label) = &arg.label {
                if keywords.iter().any(|other| other.text == label.text) {
                    return Err(label
                        .span
                        .error(format!("duplicate named argument '{}'", label.text)));
                }
                keywords.push(label);
            } else if keywords.is_empty() {
                positional += 1;
            } else {
                return Err(arg
                    .value
                    .span
                    .error("positional argument follows a named argument"));
            }
        }
        Ok(Self {
            positional,
            keywords,
        })
    }

    pub(super) fn positional(positional: usize) -> Self {
        Self {
            positional,
            keywords: vec![],
        }
    }

    pub(super) fn has_keywords(&self) -> bool {
        !self.keywords.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.positional + self.keywords.len()
    }

    pub(super) fn slots<'b>(
        &'b self,
        count: usize,
        names: impl Iterator<Item = &'b DeclaredName>,
    ) -> Result<Vec<usize>, ArgumentMismatch<'b>> {
        let named: Vec<_> = names
            .enumerate()
            .filter_map(|(index, name)| name.keyword.as_ref().map(|keyword| (index, name, keyword)))
            .collect();
        let mut slots: Vec<_> = (0..self.positional).collect();
        for label in &self.keywords {
            let Some((index, _, _)) = named.iter().find(|(_, name, _)| name.text == label.text)
            else {
                return Err(ArgumentMismatch::Unknown(label));
            };
            slots.push(*index);
        }
        if count - named.len() != self.positional {
            return Err(ArgumentMismatch::PositionalCount);
        }
        if let Some((_, name, _)) = named
            .iter()
            .find(|(index, _, keyword)| keyword.default.is_none() && !slots.contains(index))
        {
            return Err(ArgumentMismatch::Missing(&name.text));
        }
        Ok(slots)
    }
}

impl Compiler {
    pub(super) fn source_call(
        &mut self,
        name: &str,
        mut values: Vec<Value>,
        args: &[Argument],
        expected: Option<&str>,
        span: &Span,
    ) -> Result<Value, String> {
        let shape = CallArguments::new(values.len(), args)?;
        let shaped = shape.has_keywords() || self.has_keyword_candidates(name);
        for argument in args {
            let value = if shaped {
                let (types, callbacks) = self.shaped_argument_hints(
                    name,
                    &shape,
                    &values,
                    expected,
                    argument.value.omits_lambda_types(),
                );
                let hint = (types.len() == 1).then(|| types.first().unwrap().as_str());
                let callback = (callbacks.len() == 1).then(|| callbacks.first().unwrap());
                self.call_argument_with_hints(&argument.value, hint, callback)?
            } else {
                let hints = self.argument_hints(name, shape.len(), &values, expected);
                let builtin = self.enum_argument_hint(name, &values, expected);
                let hint = if hints.len() == 1 {
                    hints.first().map(String::as_str)
                } else {
                    builtin.as_deref()
                };
                self.call_argument(&argument.value, hint, name, shape.len(), &values, expected)?
            };
            values.push(value);
        }
        if !shaped {
            return self.call(name, values, expected, span);
        }
        if self.context.diverged {
            return Ok(Value::never());
        }
        let values: Vec<_> = values
            .into_iter()
            .map(|value| self.readonly_view(value, span))
            .collect();
        let Some((function, slots)) =
            self.resolve_shaped_call(name, &shape, &values, expected, span)?
        else {
            if !shape.has_keywords() {
                return self.call(name, values, expected, span);
            }
            return Err(span.error(format!(
                "no matching function '{name}' for the supplied arguments"
            )));
        };
        let values = self.complete_call_arguments(function, slots, values, span)?;
        self.emit_call(function, values, span)
    }

    pub(super) fn complete_call_arguments(
        &mut self,
        function: usize,
        slots: Vec<usize>,
        values: Vec<Value>,
        span: &Span,
    ) -> Result<Vec<Value>, String> {
        let mut ordered = vec![None; self.functions[function].params.len()];
        for (slot, value) in slots.into_iter().zip(values) {
            ordered[slot] = Some(value);
        }
        let mut values = Vec::with_capacity(ordered.len());
        for (position, value) in ordered.into_iter().enumerate() {
            if let Some(value) = value {
                values.push(value);
                continue;
            }
            let parameter = &self.keyword_parameters[&function][position];
            let Some(ParameterDefault::Function(default)) = parameter
                .keyword
                .as_ref()
                .and_then(|parameter| parameter.default.as_ref())
            else {
                unreachable!("mapped calls omit only parameters with lowered defaults")
            };
            let name = self
                .generic_instances
                .iter()
                .find(|(_, id)| **id == function)
                .map_or_else(
                    || default.text.clone(),
                    |((_, types), _)| format!("{}<{}>", default.text, types.join(",")),
                );
            let scope = self.context.declaration_scope;
            self.context.declaration_scope = self.functions[function].scope;
            let default_function = self.resolve(&name, &values, None, span);
            self.context.declaration_scope = scope;
            let default_function = default_function?.ok_or_else(|| {
                span.error(format!(
                    "cannot resolve default for named parameter '{}'",
                    self.keyword_parameters[&function][position].text
                ))
            })?;
            self.default_functions
                .entry(function)
                .or_default()
                .insert(default_function);
            values.push(self.emit_call(default_function, values.clone(), span)?);
        }
        Ok(values)
    }
}
