//! The tree as compact JSON in Hana's node and field names:
//! `{"type":"Identifier","Value":"x"}`. `tools/astdump` writes Hana's tree in
//! the same form, so equal text means equal trees.

use std::fmt::Write;

use crate::ast::*;

pub fn program(p: &Program) -> String {
    let mut w = W(String::new());
    w.node("Program", |w| w.field("Statements", |w| w.stmts(&p.statements)));
    w.0
}

struct W(String);

impl W {
    fn node(&mut self, ty: &str, fields: impl FnOnce(&mut W)) {
        self.0.push_str("{\"type\":");
        self.string(ty);
        fields(self);
        self.0.push('}');
    }

    fn field(&mut self, name: &str, value: impl FnOnce(&mut W)) {
        self.0.push(',');
        self.string(name);
        self.0.push(':');
        value(self);
    }

    fn string(&mut self, s: &str) {
        self.0.push('"');
        for c in s.chars() {
            match c {
                '"' => self.0.push_str("\\\""),
                '\\' => self.0.push_str("\\\\"),
                '\n' => self.0.push_str("\\n"),
                '\r' => self.0.push_str("\\r"),
                '\t' => self.0.push_str("\\t"),
                c if (c as u32) < 0x20 => write!(self.0, "\\u{:04x}", c as u32).unwrap(),
                c => self.0.push(c),
            }
        }
        self.0.push('"');
    }

    fn null(&mut self) {
        self.0.push_str("null");
    }

    fn bool(&mut self, b: bool) {
        self.0.push_str(if b { "true" } else { "false" });
    }

    fn list<T>(&mut self, items: &[T], mut each: impl FnMut(&mut W, &T)) {
        self.0.push('[');
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.0.push(',');
            }
            each(self, item);
        }
        self.0.push(']');
    }

    fn stmts(&mut self, s: &[Stmt]) {
        self.list(s, |w, s| w.stmt(s));
    }

    fn exprs(&mut self, e: &[Expr]) {
        self.list(e, |w, e| w.expr(e));
    }

    fn opt_expr(&mut self, e: &Option<Expr>) {
        match e {
            Some(e) => self.expr(e),
            None => self.null(),
        }
    }

    fn ident(&mut self, name: &str) {
        self.node("Identifier", |w| w.field("Value", |w| w.string(name)));
    }

    fn opt_ident(&mut self, name: &Option<String>) {
        match name {
            Some(n) => self.ident(n),
            None => self.null(),
        }
    }

    fn type_ref(&mut self, t: &TypeRef) {
        self.node("TypeReference", |w| w.field("Name", |w| w.string(&t.name)));
    }

    fn opt_type(&mut self, t: &Option<TypeRef>) {
        match t {
            Some(t) => self.type_ref(t),
            None => self.null(),
        }
    }

    fn block(&mut self, b: &Block) {
        self.node("BlockStatement", |w| w.field("Statements", |w| w.stmts(&b.statements)));
    }

    fn params(&mut self, params: &[Param]) {
        self.list(params, |w, p| {
            w.node("Parameter", |w| {
                w.field("Name", |w| w.ident(&p.name));
                w.field("TypeAnnotation", |w| w.opt_type(&p.type_annotation));
                w.field("Default", |w| w.opt_expr(&p.default));
            })
        });
    }

    fn expr(&mut self, e: &Expr) {
        match e {
            Expr::Identifier(v) => self.ident(v),
            Expr::SelfRef => self.node("SelfReference", |_| {}),
            Expr::SuperRef => self.node("SuperReference", |_| {}),
            Expr::StaticRef => self.node("StaticReference", |_| {}),
            Expr::Number(n) => self.node("NumberLiteral", |w| w.field("Value", |w| write!(w.0, "{n}").unwrap())),
            Expr::Str(s) => self.node("StringLiteral", |w| w.field("Value", |w| w.string(s))),
            Expr::Null => self.node("NullLiteral", |_| {}),
            Expr::FunctionRef(n) => self.node("FunctionReference", |w| w.field("Name", |w| w.string(n))),
            Expr::TypeRef(t) => self.type_ref(t),
            Expr::Member { object, property } => self.node("MemberExpression", |w| {
                w.field("Object", |w| w.expr(object));
                w.field("Property", |w| w.expr(property));
            }),
            Expr::Call { callee, args } => self.node("CallExpression", |w| {
                w.field("Callee", |w| w.expr(callee));
                w.field("Arguments", |w| w.exprs(args));
            }),
            Expr::New { class, args } => self.node("NewExpression", |w| {
                w.field("Class", |w| w.opt_type(class));
                w.field("Arguments", |w| w.exprs(args));
            }),
            Expr::Binary { left, op, right } | Expr::Logical { left, op, right } => {
                let ty = if matches!(e, Expr::Binary { .. }) { "BinaryExpression" } else { "LogicalExpression" };
                self.node(ty, |w| {
                    w.field("Left", |w| w.expr(left));
                    w.field("Operator", |w| w.string(op));
                    w.field("Right", |w| w.expr(right));
                })
            }
            Expr::List(items) => self.node("ListLiteral", |w| w.field("Elements", |w| w.exprs(items))),
            Expr::Dict(props) => self.node("DictLiteral", |w| {
                w.field("Properties", |w| {
                    w.list(props, |w, (k, v)| {
                        w.node("Property", |w| {
                            w.field("Key", |w| w.expr(k));
                            w.field("Value", |w| w.expr(v));
                        })
                    })
                })
            }),
            Expr::ListPop { target, position } => self.node("ListPopExpression", |w| {
                w.field("Target", |w| w.expr(target));
                w.field("Position", |w| w.string(position));
            }),
            Expr::Template(v) => self.node("TemplateLiteral", |w| w.field("Value", |w| w.string(v))),
            Expr::Bool(b) => self.node("BooleanLiteral", |w| w.field("Value", |w| w.bool(*b))),
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::VarDecl(v) => self.node("VariableDeclaration", |w| {
                w.field("Name", |w| w.opt_ident(&v.name));
                w.field("TypeRef", |w| w.opt_type(&v.type_ref));
                w.field("Value", |w| w.opt_expr(&v.value));
                w.field("IsConstant", |w| w.bool(v.is_constant));
                w.field("AccessModifier", |w| w.string(v.access));
                w.field("IsStatic", |w| w.bool(v.is_static));
                w.field("Getter", |w| match &v.getter {
                    Some(g) => w.stmts(g),
                    None => w.null(),
                });
                w.field("Setter", |w| match &v.setter {
                    Some(s) => w.node("SetterInfo", |w| {
                        w.field("Param", |w| w.opt_ident(&s.param));
                        w.field("Body", |w| w.stmts(&s.body));
                    }),
                    None => w.null(),
                });
            }),
            Stmt::Assign { target, value } => self.node("Assignment", |w| {
                w.field("Target", |w| w.expr(target));
                w.field("Value", |w| w.opt_expr(value));
            }),
            Stmt::Print { value, newline } => self.node("PrintStatement", |w| {
                w.field("Value", |w| w.expr(value));
                w.field("NewLine", |w| w.bool(*newline));
            }),
            Stmt::Input { target, type_ref } => self.node("InputStatement", |w| {
                w.field("Target", |w| w.opt_ident(target));
                w.field("TypeRef", |w| w.opt_type(type_ref));
            }),
            Stmt::Expr(e) => self.node("ExpressionStatement", |w| w.field("Expression", |w| w.expr(e))),
            Stmt::Switch { discriminant, cases } => self.node("SwitchStatement", |w| {
                w.field("Discriminant", |w| w.expr(discriminant));
                w.field("Cases", |w| {
                    w.list(cases, |w, c| {
                        w.node("SwitchCase", |w| {
                            w.field("Tests", |w| w.exprs(&c.tests));
                            w.field("Consequent", |w| w.block(&c.consequent));
                            w.field("IsDefault", |w| w.bool(c.is_default));
                        })
                    })
                });
            }),
            Stmt::Fallthrough => self.node("FallthroughStatement", |_| {}),
            Stmt::If { condition, consequent, alternate } => self.node("IfStatement", |w| {
                w.field("Condition", |w| w.expr(condition));
                w.field("Consequent", |w| w.block(consequent));
                w.field("Alternate", |w| match alternate {
                    Some(b) => w.block(b),
                    None => w.null(),
                });
            }),
            Stmt::Return(v) => self.node("ReturnStatement", |w| w.field("Value", |w| w.opt_expr(v))),
            Stmt::Break => self.node("BreakStatement", |_| {}),
            Stmt::ForEach { list, body } => self.node("ForEachLoop", |w| {
                w.field("List", |w| w.expr(list));
                w.field("Body", |w| w.block(body));
            }),
            Stmt::ForRange { start, end, loop_var, body } => self.node("ForRangeStatement", |w| {
                w.field("Start", |w| w.expr(start));
                w.field("End", |w| w.expr(end));
                w.field("LoopVar", |w| w.string(loop_var));
                w.field("Body", |w| w.block(body));
            }),
            Stmt::While { condition, body } => self.node("WhileLoop", |w| {
                w.field("Condition", |w| w.expr(condition));
                w.field("Body", |w| w.block(body));
            }),
            Stmt::Class { name, base, interfaces, body, is_abstract } => self.node("ClassDeclaration", |w| {
                w.field("Name", |w| w.opt_type(name));
                w.field("BaseClass", |w| w.opt_type(base));
                w.field("Interfaces", |w| w.list(interfaces, |w, t| w.type_ref(t)));
                w.field("Body", |w| w.stmts(body));
                w.field("IsAbstract", |w| w.bool(*is_abstract));
            }),
            Stmt::Import { module, is_builtin, all, items } => self.node("ImportStatement", |w| {
                w.field("Module", |w| w.string(module));
                w.field("IsBuiltin", |w| w.bool(*is_builtin));
                w.field("All", |w| w.bool(*all));
                w.field("Items", |w| {
                    w.list(items, |w, i| {
                        w.node("ImportItem", |w| {
                            w.field("Name", |w| w.string(&i.name));
                            w.field("As", |w| w.string(&i.alias));
                        })
                    })
                });
            }),
            Stmt::Interface { name, body } => self.node("InterfaceDeclaration", |w| {
                w.field("Name", |w| w.opt_type(name));
                w.field("Body", |w| w.stmts(body));
            }),
            Stmt::ListPush { target, value, position } => self.node("ListPushStatement", |w| {
                w.field("Target", |w| w.expr(target));
                w.field("Value", |w| w.expr(value));
                w.field("Position", |w| w.string(position));
            }),
            Stmt::ListPop { target, position } => self.node("ListPopStatement", |w| {
                w.field("Target", |w| w.expr(target));
                w.field("Position", |w| w.string(position));
            }),
            Stmt::Try { block, handlers, finalizer } => self.node("TryStatement", |w| {
                w.field("Block", |w| w.block(block));
                w.field("Handlers", |w| {
                    w.list(handlers, |w, h| {
                        w.node("CatchClause", |w| {
                            w.field("Type", |w| w.opt_type(&h.type_ref));
                            w.field("Param", |w| w.ident(&h.param));
                            w.field("Body", |w| w.block(&h.body));
                        })
                    })
                });
                w.field("Finalizer", |w| match finalizer {
                    Some(b) => w.block(b),
                    None => w.null(),
                });
            }),
            Stmt::Throw(v) => self.node("ThrowStatement", |w| w.field("Value", |w| w.expr(v))),
            Stmt::Function(f) => self.node("FunctionDeclaration", |w| {
                w.field("Name", |w| w.ident(&f.name));
                w.field("Params", |w| w.params(&f.params));
                w.field("Body", |w| w.block(&f.body));
                w.field("AccessModifier", |w| w.string(f.access));
                w.field("IsStatic", |w| w.bool(f.is_static));
                w.field("ReturnType", |w| w.opt_type(&f.return_type));
            }),
            Stmt::InterfaceMethod(name) => self.node("InterfaceMethod", |w| w.field("Name", |w| w.ident(name))),
            Stmt::Constructor { id, params, body } => self.node("ConstructorDeclaration", |w| {
                w.field("Id", |w| w.ident(id));
                w.field("Params", |w| w.params(params));
                w.field("Body", |w| w.stmts(body));
            }),
        }
    }
}
