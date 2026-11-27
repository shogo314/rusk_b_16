//! A function name denotes a static family, with invocation entries generated
//! for its actual argument types. Aliases keep the original lookup origin.
use super::{arguments::CallArguments, *};
use callables::Callable;

#[derive(Clone)]
pub(super) struct Family {
    pub name: Name,
    pub origin: scopes::ScopeId,
    pub anchor: usize,
    pub concrete: BTreeSet<usize>,
    pub generic: BTreeSet<usize>,
    pub entries: BTreeMap<(usize, Vec<usize>), Callable>,
}
impl Family {
    pub(super) fn accepts_instance(&self, template: usize, arguments: &[String]) -> bool {
        self.generic.contains(&template)
            && tuples::type_parts(&self.name.text).is_none_or(|(_, explicit)| {
                explicit
                    .iter()
                    .copied()
                    .eq(arguments.iter().map(String::as_str))
            })
    }
}

impl Compiler {
    pub(super) fn callable_argument_hint(
        &mut self,
        ty: &str,
        count: usize,
        known: &[Value],
        expected: Option<&str>,
    ) -> Option<String> {
        let Some(family) = self.families.get(ty).cloned() else {
            return self.callables.get(ty)?.parameters.get(known.len()).cloned();
        };
        let previous = self.context.declaration_scope;
        self.context.declaration_scope = family.origin;
        let hints = self.argument_hints(&family.name.text, count, known, expected);
        self.context.declaration_scope = previous;
        (hints.len() == 1).then(|| hints.into_iter().next().unwrap())
    }

    pub(super) fn family_value(&mut self, name: &Name) -> Result<Value, String> {
        let explicit = tuples::type_parts(&name.text)
            .map(|(_, args)| args.into_iter().map(String::from).collect::<Vec<_>>());
        if let Some(arguments) = &explicit {
            for argument in arguments {
                if generics::constant_kind(argument).is_some() {
                    continue;
                }
                self.check_type(&Name {
                    constant: None,
                    text: argument.clone(),
                    span: name.span.clone(),
                })?;
            }
        }
        let scopes = self.function_scopes(&name.text, &[]);
        let lookup = self.source_function_name(&name.text);
        let base = tuples::type_parts(&lookup).map_or(lookup.as_ref(), |(base, _)| base);
        let concrete = self
            .functions
            .iter()
            .enumerate()
            .filter(|(id, f)| {
                scopes.contains(&f.scope)
                    && f.name.text == lookup
                    && !self.generic_calls.contains_key(id)
            })
            .map(|(id, _)| id)
            .collect::<BTreeSet<_>>();
        let generic = self
            .generic_functions
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                scopes.contains(&f.scope)
                    && f.name.text == base
                    && explicit.as_ref().is_none_or(|_| {
                        generics::explicit_bindings(&name.text, &f.parameters).is_some()
                    })
            })
            .map(|(id, _)| id)
            .collect::<BTreeSet<_>>();
        if concrete.is_empty() && generic.is_empty() {
            return Err(name
                .span
                .error(format!("unknown variable or function '{}'", name.text)));
        }
        let key = (name.text.clone(), scopes.iter().copied().collect());
        if let Some(ty) = self.family_types.get(&key).cloned() {
            return Ok(self.construct_callable(&ty, vec![], &name.span));
        }
        if self.callables.len() >= 256 {
            return Err(name.span.error("callable specialization limit exceeded"));
        }
        let ty = format!("__RuskCallable{}", self.lambda_counter);
        self.lambda_counter += 1;
        let native = format!("RuskRecord{}", self.records.len());
        self.types.insert(ty.clone(), native);
        self.records.insert(
            ty.clone(),
            records::Record {
                name: Name {
                    constant: None,
                    text: ty.clone(),
                    span: name.span.clone(),
                },
                fields: vec![],
            },
        );
        self.prepare_record(&ty)?;
        let anchor = self.functions.len();
        self.functions.push(Function {
            scope: scopes::ScopeId::ROOT,
            name: Name {
                constant: None,
                text: format!("$family{anchor}"),
                span: name.span.clone(),
            },
            params: vec![ty.clone()],
            result: "Unit".into(),
            cpp: format!("rusk_family_{anchor}"),
            code: String::new(),
            body: Some((vec![], Box::new(Stmt::Block(vec![], None)))),
            origin: Origin::Generated,
            checked: Some(Body {
                instructions: vec![Instruction::Return {
                    value: None,
                    span: Some(name.span.clone()),
                }],
            }),
        });
        self.environments
            .closures
            .insert(anchor, local_captures::ClosureLayout::new(ty.clone(), 0));
        self.callables.insert(
            ty.clone(),
            Callable {
                parameters: vec![],
                result: "Unit".into(),
                function: anchor,
            },
        );
        self.families.insert(
            ty.clone(),
            Family {
                name: name.clone(),
                origin: *scopes.first().unwrap(),
                anchor,
                concrete,
                generic,
                entries: BTreeMap::new(),
            },
        );
        self.family_types.insert(key, ty.clone());
        Ok(self.construct_callable(&ty, vec![], &name.span))
    }

    pub(super) fn specialize_callable(
        &mut self,
        ty: &str,
        arguments: &[String],
        expected: Option<&str>,
        span: &Span,
    ) -> Result<Callable, String> {
        if let Some(probe) = &mut self.lambda_probe
            && probe.ty == ty
        {
            probe.signature = Some(arguments.to_vec());
            return Err(span.error("reached the inferred lambda invocation"));
        }
        let Some(family) = self.families.get(ty).cloned() else {
            return self
                .callables
                .get(ty)
                .cloned()
                .ok_or_else(|| span.error(format!("type '{ty}' is not statically callable")));
        };
        let values = arguments
            .iter()
            .enumerate()
            .map(|(i, ty)| Value {
                id: ValueId::Parameter(i + 1),
                ty: ty.clone(),
            })
            .collect::<Vec<_>>();
        let shape = CallArguments::positional(arguments.len());
        self.specialize_family(&family, ty, &shape, values, expected, span)
    }

    pub(super) fn invoke_shaped_family(
        &mut self,
        callee: Value,
        family: &Family,
        mut values: Vec<Value>,
        arguments: &[Argument],
        expected: Option<&str>,
        span: &Span,
    ) -> Result<Value, String> {
        let shape = CallArguments::new(values.len(), arguments)?;
        for argument in arguments {
            let previous = self.context.declaration_scope;
            self.context.declaration_scope = family.origin;
            let (types, callbacks) = self.shaped_argument_hints(
                &family.name.text,
                &shape,
                &values,
                expected,
                argument.value.omits_lambda_types(),
            );
            self.context.declaration_scope = previous;
            let hint = (types.len() == 1).then(|| types.first().unwrap().as_str());
            let signature = (callbacks.len() == 1).then(|| callbacks.first().unwrap());
            values.push(self.call_argument_with_hints(&argument.value, hint, signature)?);
        }
        if self.context.diverged {
            return Ok(Value::never());
        }
        let entry =
            self.specialize_family(family, &callee.ty, &shape, values.clone(), expected, span)?;
        let args = std::iter::once(callee).chain(values).collect();
        self.emit_call(entry.function, args, span)
    }

    fn specialize_family(
        &mut self,
        family: &Family,
        ty: &str,
        shape: &CallArguments<'_>,
        arguments: Vec<Value>,
        expected: Option<&str>,
        span: &Span,
    ) -> Result<Callable, String> {
        let previous = self.context.declaration_scope;
        self.context.declaration_scope = family.origin;
        let target = if shape.has_keywords() || self.has_keyword_candidates(&family.name.text) {
            self.resolve_shaped_call(&family.name.text, shape, &arguments, expected, span)
        } else {
            self.resolve_callable(&family.name.text, &arguments, expected, span)
                .map(|id| id.map(|id| (id, (0..arguments.len()).collect())))
        };
        self.context.declaration_scope = previous;
        let Some((target, slots)) = target? else {
            return Err(span.error(format!(
                "no matching function '{}' for ({})",
                family.name.text,
                arguments
                    .iter()
                    .map(|argument| argument.ty.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        };
        let key = (target, slots.clone());
        if let Some(entry) = family.entries.get(&key) {
            return Ok(entry.clone());
        }
        if family.entries.len() >= 256 {
            return Err(span.error("callable specialization limit exceeded"));
        }
        let parameters: Vec<_> = arguments.into_iter().map(|argument| argument.ty).collect();
        let result = self.functions[target].result.clone();
        let index = self.functions.len();
        self.functions.push(Function {
            scope: scopes::ScopeId::ROOT,
            name: Name {
                constant: None,
                text: format!("$invoke{index}"),
                span: span.clone(),
            },
            params: std::iter::once(ty.to_string())
                .chain(parameters.iter().cloned())
                .collect(),
            result: result.clone(),
            cpp: format!("rusk_callable_{index}"),
            code: String::new(),
            body: Some((vec![], Box::new(Stmt::Block(vec![], None)))),
            origin: Origin::Generated,
            checked: None,
        });
        let (checked, completed) = self.with_function_context(
            FunctionContext::function(index, result.clone()),
            |compiler| {
                let values = parameters
                    .iter()
                    .enumerate()
                    .map(|(i, ty)| Value {
                        id: ValueId::Parameter(i + 1),
                        ty: ty.clone(),
                    })
                    .collect();
                let values = compiler.complete_call_arguments(target, slots, values, span)?;
                let value = compiler.emit_call(target, values, span)?;
                for instruction in &mut compiler.context.body.instructions {
                    if let Instruction::Call(call) = instruction {
                        call.inherit_location = true;
                    }
                }
                compiler.context.body.push(Instruction::Return {
                    value: (access_types::underlying(&result) != "Unit").then_some(value),
                    span: Some(span.clone()),
                });
                Ok::<_, String>(())
            },
        );
        checked?;
        self.functions[index].checked = Some(completed.body);
        self.environments
            .closures
            .insert(index, local_captures::ClosureLayout::new(ty.into(), 0));
        let entry = Callable {
            parameters,
            result,
            function: index,
        };
        self.families
            .get_mut(ty)
            .unwrap()
            .entries
            .insert(key, entry.clone());
        Ok(entry)
    }
}
