use crate::internal::arrays;
use crate::internal::attributes;
use crate::internal::classes;
use crate::internal::control_flow;
use crate::internal::functions;
use crate::internal::identifiers;
use crate::internal::names;
use crate::internal::parameters;
use crate::internal::precedences::Associativity;
use crate::internal::precedences::Precedence;
use crate::internal::strings;
use crate::internal::utils;
use crate::internal::variables;
use crate::state::State;
use crate::ParserDiagnostic;
use pxp_ast::Expression;
use pxp_ast::*;
use pxp_ast::{
    ArrayIndexExpression, CoalesceExpression, ConcatExpression, ConstantFetchExpression,
    ExpressionKind, FunctionCallExpression, FunctionClosureCreationExpression,
    InstanceofExpression, MagicConstantExpression, MethodCallExpression,
    MethodClosureCreationExpression, NullsafeMethodCallExpression, NullsafePropertyFetchExpression,
    PropertyFetchExpression, ReferenceExpression, ShortTernaryExpression,
    StaticMethodCallExpression, StaticMethodClosureCreationExpression,
    StaticPropertyFetchExpression, StaticVariableMethodCallExpression,
    StaticVariableMethodClosureCreationExpression, TernaryExpression,
};

use pxp_diagnostics::Severity;
use pxp_span::Span;
use pxp_syntax::comments::CommentGroup;
use pxp_token::TokenKind;

use pxp_ast::BoolExpression;
use pxp_ast::CastExpression;
use pxp_ast::CloneExpression;
use pxp_ast::DieExpression;
use pxp_ast::EmptyExpression;
use pxp_ast::ErrorSuppressExpression;
use pxp_ast::EvalExpression;
use pxp_ast::ExitExpression;
use pxp_ast::IncludeExpression;
use pxp_ast::IncludeOnceExpression;
use pxp_ast::IssetExpression;
use pxp_ast::NewExpression;
use pxp_ast::ParenthesizedExpression;
use pxp_ast::PrintExpression;
use pxp_ast::RequireExpression;
use pxp_ast::RequireOnceExpression;
use pxp_ast::ThrowExpression;
use pxp_ast::UnsetExpression;
use pxp_ast::YieldExpression;
use pxp_ast::YieldFromExpression;

pub fn create(state: &mut State) -> Expression {
    for_precedence(state, Precedence::Lowest)
}

fn null_coalesce_precedence(state: &mut State) -> Expression {
    for_precedence(state, Precedence::NullCoalesce)
}

fn clone_or_new_precedence(state: &mut State) -> Expression {
    for_precedence(state, Precedence::CloneOrNew)
}

fn for_precedence(state: &mut State, precedence: Precedence) -> Expression {
    let mut left = left(state, &precedence);

    loop {
        let current = state.stream.current();
        let span = current.span;
        let kind = &current.kind;

        if matches!(current.kind, TokenKind::SemiColon | TokenKind::Eof) {
            break;
        }

        if is_postfix(kind) {
            let lpred = Precedence::postfix(kind);

            if lpred < precedence {
                break;
            }

            left = postfix(state, left, kind);
            continue;
        }

        if is_infix(kind) {
            let rpred = Precedence::infix(kind);

            if rpred < precedence {
                break;
            }

            if rpred == precedence && matches!(rpred.associativity(), Some(Associativity::Left)) {
                break;
            }

            if rpred == precedence && matches!(rpred.associativity(), Some(Associativity::Non)) {
                state.diagnostic(
                    ParserDiagnostic::UnexpectedToken { token: *current },
                    Severity::Error,
                    current.span,
                );
            }

            state.stream.next();

            let op = state.stream.current();
            let start_span = op.span;
            let kind = match kind {
                TokenKind::Question => {
                    // this happens due to a comment, or whitespaces between the  and the :
                    // we consider `foo()  : bar()` a ternary expression, with `then` being a noop
                    // however, this must behave like a short ternary at runtime.
                    if op.kind == TokenKind::Colon {
                        state.stream.next();

                        let r#else = create(state);

                        ExpressionKind::Ternary(TernaryExpression {
                            span: Span::combine(left.span, r#else.span),
                            condition: Box::new(left),
                            question: span,
                            then: Box::new(Expression::noop(start_span)),
                            colon: op.span,
                            r#else: Box::new(r#else),
                        })
                    } else {
                        let then = create(state);
                        let colon = utils::skip_colon(state);
                        let r#else = create(state);

                        ExpressionKind::Ternary(TernaryExpression {
                            span: Span::combine(left.span, r#else.span),
                            condition: Box::new(left),
                            question: span,
                            then: Box::new(then),
                            colon,
                            r#else: Box::new(r#else),
                        })
                    }
                }
                TokenKind::QuestionColon => {
                    let r#else = create(state);
                    ExpressionKind::ShortTernary(ShortTernaryExpression {
                        span: Span::combine(left.span, r#else.span),
                        condition: Box::new(left),
                        question_colon: span,
                        r#else: Box::new(r#else),
                    })
                }
                TokenKind::Equals if op.kind == TokenKind::Ampersand => {
                    state.stream.next();

                    // FIXME: You should only be allowed to assign a referencable variable,
                    //        here, not any old expression.
                    let right = Box::new(for_precedence(state, rpred));
                    let right_span = right.span;
                    let span = Span::combine(left.span, right_span);
                    let reference_span = Span::combine(op.span, right_span);

                    ExpressionKind::AssignmentOperation(AssignmentOperationExpression {
                        span,
                        kind: AssignmentOperationKind::Assign {
                            left: Box::new(left),
                            equals: span,
                            right: Box::new(Expression::new(
                                ExpressionKind::Reference(ReferenceExpression {
                                    span: reference_span,
                                    ampersand: op.span,
                                    right,
                                }),
                                Span::new(start_span.start, right_span.end),
                                CommentGroup::default(),
                            )),
                        },
                    })
                }
                TokenKind::Instanceof if op.kind == TokenKind::Self_ => {
                    let self_span = op.span;
                    state.stream.next();
                    let right = Expression::new(
                        ExpressionKind::Self_(self_span),
                        self_span,
                        CommentGroup::default(),
                    );
                    let span = Span::combine(left.span, right.span);

                    ExpressionKind::Instanceof(InstanceofExpression {
                        span,
                        left: Box::new(left),
                        instanceof: span,
                        right: Box::new(right),
                    })
                }
                TokenKind::Instanceof if op.kind == TokenKind::Parent => {
                    let instanceof = span;
                    state.stream.next();
                    let right = Expression::new(
                        ExpressionKind::Parent(op.span),
                        op.span,
                        CommentGroup::default(),
                    );
                    let span = Span::combine(left.span, right.span);

                    ExpressionKind::Instanceof(InstanceofExpression {
                        span,
                        left: Box::new(left),
                        instanceof: span,
                        right: Box::new(right),
                    })
                }
                TokenKind::Instanceof if op.kind == TokenKind::Static => {
                    let instanceof = span;
                    state.stream.next();
                    let right = Expression::new(
                        ExpressionKind::Static(op.span),
                        op.span,
                        CommentGroup::default(),
                    );

                    ExpressionKind::Instanceof(InstanceofExpression {
                        span: Span::combine(left.span, right.span),
                        left: Box::new(left),
                        instanceof,
                        right: Box::new(right),
                    })
                }
                TokenKind::Instanceof if op.kind == TokenKind::Enum => {
                    let enum_span = op.span;
                    state.stream.next();

                    let right = Expression::new(
                        ExpressionKind::Identifier(Identifier::SimpleIdentifier(
                            SimpleIdentifier::new(op.symbol.unwrap(), enum_span),
                        )),
                        enum_span,
                        CommentGroup::default(),
                    );

                    ExpressionKind::Instanceof(InstanceofExpression {
                        span: Span::combine(left.span, right.span),
                        left: Box::new(left),
                        instanceof: span,
                        right: Box::new(right),
                    })
                }
                TokenKind::Instanceof if op.kind == TokenKind::From => {
                    let from_span = op.span;
                    state.stream.next();
                    let right = Expression::new(
                        ExpressionKind::Identifier(Identifier::SimpleIdentifier(
                            SimpleIdentifier::new(op.symbol.unwrap(), op.span),
                        )),
                        Span::new(start_span.start, from_span.end),
                        CommentGroup::default(),
                    );

                    ExpressionKind::Instanceof(InstanceofExpression {
                        span: Span::combine(left.span, right.span),
                        left: Box::new(left),
                        instanceof: span,
                        right: Box::new(right),
                    })
                }
                _ => {
                    let op_span = span;
                    let left = Box::new(left);
                    let right = Box::new(for_precedence(state, rpred));
                    let span = Span::combine(left.span, right.span);

                    match kind {
                        TokenKind::Plus => {
                            ExpressionKind::ArithmeticOperation(ArithmeticOperationExpression {
                                span,
                                kind: ArithmeticOperationKind::Addition {
                                    left,
                                    plus: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::Minus => {
                            ExpressionKind::ArithmeticOperation(ArithmeticOperationExpression {
                                span,
                                kind: ArithmeticOperationKind::Subtraction {
                                    left,
                                    minus: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::Asterisk => {
                            ExpressionKind::ArithmeticOperation(ArithmeticOperationExpression {
                                span,
                                kind: ArithmeticOperationKind::Multiplication {
                                    left,
                                    asterisk: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::Slash => {
                            ExpressionKind::ArithmeticOperation(ArithmeticOperationExpression {
                                span,
                                kind: ArithmeticOperationKind::Division {
                                    left,
                                    slash: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::Percent => {
                            ExpressionKind::ArithmeticOperation(ArithmeticOperationExpression {
                                span,
                                kind: ArithmeticOperationKind::Modulo {
                                    left,
                                    percent: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::Pow => {
                            ExpressionKind::ArithmeticOperation(ArithmeticOperationExpression {
                                span,
                                kind: ArithmeticOperationKind::Exponentiation {
                                    left,
                                    pow: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::Equals => {
                            ExpressionKind::AssignmentOperation(AssignmentOperationExpression {
                                span,
                                kind: AssignmentOperationKind::Assign {
                                    left,
                                    equals: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::PlusEquals => {
                            ExpressionKind::AssignmentOperation(AssignmentOperationExpression {
                                span,
                                kind: AssignmentOperationKind::Addition {
                                    left,
                                    plus_equals: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::MinusEquals => {
                            ExpressionKind::AssignmentOperation(AssignmentOperationExpression {
                                span,
                                kind: AssignmentOperationKind::Subtraction {
                                    left,
                                    minus_equals: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::AsteriskEquals => {
                            ExpressionKind::AssignmentOperation(AssignmentOperationExpression {
                                span,
                                kind: AssignmentOperationKind::Multiplication {
                                    left,
                                    asterisk_equals: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::SlashEquals => {
                            ExpressionKind::AssignmentOperation(AssignmentOperationExpression {
                                span,
                                kind: AssignmentOperationKind::Division {
                                    left,
                                    slash_equals: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::PercentEquals => {
                            ExpressionKind::AssignmentOperation(AssignmentOperationExpression {
                                span,
                                kind: AssignmentOperationKind::Modulo {
                                    left,
                                    percent_equals: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::PowEquals => {
                            ExpressionKind::AssignmentOperation(AssignmentOperationExpression {
                                span,
                                kind: AssignmentOperationKind::Exponentiation {
                                    left,
                                    pow_equals: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::AmpersandEquals => {
                            ExpressionKind::AssignmentOperation(AssignmentOperationExpression {
                                span,
                                kind: AssignmentOperationKind::BitwiseAnd {
                                    left,
                                    ampersand_equals: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::PipeEquals => {
                            ExpressionKind::AssignmentOperation(AssignmentOperationExpression {
                                span,
                                kind: AssignmentOperationKind::BitwiseOr {
                                    left,
                                    pipe_equals: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::CaretEquals => {
                            ExpressionKind::AssignmentOperation(AssignmentOperationExpression {
                                span,
                                kind: AssignmentOperationKind::BitwiseXor {
                                    left,
                                    caret_equals: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::LeftShiftEquals => {
                            ExpressionKind::AssignmentOperation(AssignmentOperationExpression {
                                span,
                                kind: AssignmentOperationKind::LeftShift {
                                    left,
                                    left_shift_equals: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::RightShiftEquals => {
                            ExpressionKind::AssignmentOperation(AssignmentOperationExpression {
                                span,
                                kind: AssignmentOperationKind::RightShift {
                                    left,
                                    right_shift_equals: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::DoubleQuestionEquals => {
                            ExpressionKind::AssignmentOperation(AssignmentOperationExpression {
                                span,
                                kind: AssignmentOperationKind::Coalesce {
                                    left,
                                    coalesce_equals: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::DotEquals => {
                            ExpressionKind::AssignmentOperation(AssignmentOperationExpression {
                                span,
                                kind: AssignmentOperationKind::Concat {
                                    left,
                                    dot_equals: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::Ampersand => {
                            ExpressionKind::BitwiseOperation(BitwiseOperationExpression {
                                span,
                                kind: BitwiseOperationKind::And {
                                    left,
                                    and: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::Pipe => {
                            ExpressionKind::BitwiseOperation(BitwiseOperationExpression {
                                span,
                                kind: BitwiseOperationKind::Or {
                                    left,
                                    or: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::Caret => {
                            ExpressionKind::BitwiseOperation(BitwiseOperationExpression {
                                span,
                                kind: BitwiseOperationKind::Xor {
                                    left,
                                    xor: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::LeftShift => {
                            ExpressionKind::BitwiseOperation(BitwiseOperationExpression {
                                span,
                                kind: BitwiseOperationKind::LeftShift {
                                    left,
                                    left_shift: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::RightShift => {
                            ExpressionKind::BitwiseOperation(BitwiseOperationExpression {
                                span,
                                kind: BitwiseOperationKind::RightShift {
                                    left,
                                    right_shift: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::DoubleEquals => {
                            ExpressionKind::ComparisonOperation(ComparisonOperationExpression {
                                span,
                                kind: ComparisonOperationKind::Equal {
                                    left,
                                    double_equals: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::TripleEquals => {
                            ExpressionKind::ComparisonOperation(ComparisonOperationExpression {
                                span,
                                kind: ComparisonOperationKind::Identical {
                                    left,
                                    triple_equals: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::BangEquals => {
                            ExpressionKind::ComparisonOperation(ComparisonOperationExpression {
                                span,
                                kind: ComparisonOperationKind::NotEqual {
                                    left,
                                    bang_equals: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::AngledLeftRight => {
                            ExpressionKind::ComparisonOperation(ComparisonOperationExpression {
                                span,
                                kind: ComparisonOperationKind::AngledNotEqual {
                                    left,
                                    angled_left_right: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::BangDoubleEquals => {
                            ExpressionKind::ComparisonOperation(ComparisonOperationExpression {
                                span,
                                kind: ComparisonOperationKind::NotIdentical {
                                    left,
                                    bang_double_equals: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::LessThan => {
                            ExpressionKind::ComparisonOperation(ComparisonOperationExpression {
                                span,
                                kind: ComparisonOperationKind::LessThan {
                                    left,
                                    less_than: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::GreaterThan => {
                            ExpressionKind::ComparisonOperation(ComparisonOperationExpression {
                                span,
                                kind: ComparisonOperationKind::GreaterThan {
                                    left,
                                    greater_than: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::LessThanEquals => {
                            ExpressionKind::ComparisonOperation(ComparisonOperationExpression {
                                span,
                                kind: ComparisonOperationKind::LessThanOrEqual {
                                    left,
                                    less_than_equals: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::GreaterThanEquals => {
                            ExpressionKind::ComparisonOperation(ComparisonOperationExpression {
                                span,
                                kind: ComparisonOperationKind::GreaterThanOrEqual {
                                    left,
                                    greater_than_equals: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::Spaceship => {
                            ExpressionKind::ComparisonOperation(ComparisonOperationExpression {
                                span,
                                kind: ComparisonOperationKind::Spaceship {
                                    left,
                                    spaceship: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::BooleanAnd => {
                            ExpressionKind::LogicalOperation(LogicalOperationExpression {
                                span,
                                kind: LogicalOperationKind::And {
                                    left,
                                    double_ampersand: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::BooleanOr => {
                            ExpressionKind::LogicalOperation(LogicalOperationExpression {
                                span,
                                kind: LogicalOperationKind::Or {
                                    left,
                                    double_pipe: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::LogicalAnd => {
                            ExpressionKind::LogicalOperation(LogicalOperationExpression {
                                span,
                                kind: LogicalOperationKind::LogicalAnd {
                                    left,
                                    and: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::LogicalOr => {
                            ExpressionKind::LogicalOperation(LogicalOperationExpression {
                                span,
                                kind: LogicalOperationKind::LogicalOr {
                                    left,
                                    or: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::LogicalXor => {
                            ExpressionKind::LogicalOperation(LogicalOperationExpression {
                                span,
                                kind: LogicalOperationKind::LogicalXor {
                                    left,
                                    xor: op_span,
                                    right,
                                },
                            })
                        }
                        TokenKind::Dot => ExpressionKind::Concat(ConcatExpression {
                            span,
                            left,
                            dot: op_span,
                            right,
                        }),
                        TokenKind::Instanceof => ExpressionKind::Instanceof(InstanceofExpression {
                            span,
                            left,
                            instanceof: op_span,
                            right,
                        }),
                        _ => unreachable!(),
                    }
                }
            };

            let end_span = state.stream.previous().span;

            left = Expression::new(
                kind,
                Span::new(start_span.start, end_span.end),
                CommentGroup::default(),
            );

            continue;
        }

        break;
    }

    left
}

pub fn attributes(state: &mut State) -> Expression {
    attributes::gather_attributes(state);

    let current = state.stream.current();

    match &current.kind {
        TokenKind::Static if state.stream.peek().kind == TokenKind::Function => {
            functions::anonymous_function(state)
        }
        TokenKind::Static if state.stream.peek().kind == TokenKind::Fn => {
            functions::arrow_function(state)
        }
        TokenKind::Function => functions::anonymous_function(state),
        TokenKind::Fn => functions::arrow_function(state),
        _ => {
            state.diagnostic(
                ParserDiagnostic::InvalidTargetForAttributes,
                Severity::Error,
                current.span,
            );

            Expression::missing(current.span)
        }
    }
}

fn left(state: &mut State, precedence: &Precedence) -> Expression {
    if state.stream.is_eof() {
        state.diagnostic(
            ParserDiagnostic::UnexpectedEndOfFile,
            Severity::Error,
            state.stream.current().span,
        );

        return Expression::missing(state.stream.current().span);
    }

    let current = state.stream.current();
    let peek = state.stream.peek();

    match (&current.kind, &peek.kind) {
        (TokenKind::Attribute, _) => attributes(state),

        (TokenKind::Static, TokenKind::Fn) => functions::arrow_function(state),

        (TokenKind::Static, TokenKind::Function) => functions::anonymous_function(state),

        (TokenKind::Fn, _) => functions::arrow_function(state),

        (TokenKind::Function, _) => functions::anonymous_function(state),

        (TokenKind::Eval, TokenKind::LeftParen) => {
            let start_span = state.stream.current().span;
            let eval = state.stream.current().span;
            state.stream.next();

            let argument = Box::new(parameters::single_argument(state, true, true).unwrap());
            let end_span = state.stream.previous().span;

            Expression::new(
                ExpressionKind::Eval(EvalExpression { span: Span::combine(start_span, end_span), eval, argument }),
                Span::new(start_span.start, end_span.end),
                CommentGroup::default(),
            )
        }

        (TokenKind::Empty, TokenKind::LeftParen) => {
            let start_span = state.stream.current().span;
            let empty = state.stream.current().span;
            state.stream.next();

            let argument = Box::new(parameters::single_argument(state, true, true).unwrap());
            let end_span = state.stream.previous().span;

            Expression::new(
                ExpressionKind::Empty(EmptyExpression { empty, argument }),
                Span::new(start_span.start, end_span.end),
                CommentGroup::default(),
            )
        }

        (TokenKind::Die, _) => {
            let start_span = state.stream.current().span;
            let die = state.stream.current().span;
            state.stream.next();

            let argument = parameters::single_argument(state, false, true).map(Box::new);

            let end_span = state.stream.previous().span;

            Expression::new(
                ExpressionKind::Die(DieExpression { die, argument }),
                Span::new(start_span.start, end_span.end),
                CommentGroup::default(),
            )
        }

        (TokenKind::Exit, _) => {
            let start_span = state.stream.current().span;
            let exit = state.stream.current().span;
            state.stream.next();

            let argument = parameters::single_argument(state, false, true).map(Box::new);

            let end_span = state.stream.previous().span;

            Expression::new(
                ExpressionKind::Exit(ExitExpression { exit, argument }),
                Span::new(start_span.start, end_span.end),
                CommentGroup::default(),
            )
        }

        (TokenKind::Isset, TokenKind::LeftParen) => {
            let start_span = state.stream.current().span;
            let isset = state.stream.current().span;
            state.stream.next();
            let arguments = parameters::argument_list(state);
            let end_span = state.stream.previous().span;

            Expression::new(
                ExpressionKind::Isset(IssetExpression { isset, arguments }),
                Span::new(start_span.start, end_span.end),
                CommentGroup::default(),
            )
        }

        (TokenKind::Unset, TokenKind::LeftParen) => {
            let start_span = state.stream.current().span;
            let unset = state.stream.current().span;
            state.stream.next();
            let arguments = parameters::argument_list(state);
            let end_span = state.stream.previous().span;

            Expression::new(
                ExpressionKind::Unset(UnsetExpression { unset, arguments }),
                Span::new(start_span.start, end_span.end),
                CommentGroup::default(),
            )
        }

        (TokenKind::Print, _) => {
            let start_span = state.stream.current().span;
            let print = state.stream.current().span;
            state.stream.next();

            let mut value = None;
            let mut argument = None;

            if let Some(arg) = parameters::single_argument(state, false, true) {
                argument = Some(Box::new(arg));
            } else {
                value = Some(Box::new(create(state)));
            }

            let end_span = state.stream.previous().span;

            Expression::new(
                ExpressionKind::Print(PrintExpression {
                    print,
                    value,
                    argument,
                }),
                Span::new(start_span.start, end_span.end),
                CommentGroup::default(),
            )
        }

        (
            TokenKind::True
            | TokenKind::False
            | TokenKind::Null
            | TokenKind::Readonly
            | TokenKind::Self_
            | TokenKind::Parent
            | TokenKind::Enum
            | TokenKind::From,
            TokenKind::LeftParen,
        ) => {
            let name = names::name_maybe_soft_reserved(state, UseKind::Function);

            let lhs = Expression::new(
                ExpressionKind::Name(name),
                name.span,
                CommentGroup::default(),
            );

            postfix(state, lhs, &TokenKind::LeftParen)
        }

        (TokenKind::Enum | TokenKind::From, TokenKind::DoubleColon) => {
            let name = names::full_name_including_self(state);
            let lhs = Expression::new(
                ExpressionKind::Name(name),
                name.span,
                CommentGroup::default(),
            );

            postfix(state, lhs, &TokenKind::DoubleColon)
        }

        (TokenKind::List, _) => arrays::list_expression(state),

        (TokenKind::New, TokenKind::Class | TokenKind::Attribute) => {
            classes::parse_anonymous(state, None)
        }

        (TokenKind::Throw, _) => {
            let start_span = state.stream.current().span;
            state.stream.next();
            let exception = for_precedence(state, Precedence::Lowest);
            let exception_span = exception.span;

            Expression::new(
                ExpressionKind::Throw(ThrowExpression {
                    value: Box::new(exception),
                }),
                Span::new(start_span.start, exception_span.end),
                CommentGroup::default(),
            )
        }

        (TokenKind::Yield, _) => {
            let start_span = state.stream.current().span;
            state.stream.next();
            if state.stream.current().kind == TokenKind::SemiColon
                || state.stream.current().kind == TokenKind::RightParen
            {
                Expression::new(
                    ExpressionKind::Yield(YieldExpression {
                        key: None,
                        value: None,
                    }),
                    start_span,
                    CommentGroup::default(),
                )
            } else {
                let mut from = false;

                if state.stream.current().kind == TokenKind::From {
                    state.stream.next();
                    from = true;
                }

                let mut key = None;
                let mut value = Box::new(for_precedence(
                    state,
                    if from {
                        Precedence::YieldFrom
                    } else {
                        Precedence::Yield
                    },
                ));

                if state.stream.current().kind == TokenKind::DoubleArrow && !from {
                    state.stream.next();
                    key = Some(value.clone());
                    value = Box::new(for_precedence(state, Precedence::Yield));
                }

                let end_span = state.stream.previous().span;

                if from {
                    Expression::new(
                        ExpressionKind::YieldFrom(YieldFromExpression { value }),
                        Span::new(start_span.start, end_span.end),
                        CommentGroup::default(),
                    )
                } else {
                    Expression::new(
                        ExpressionKind::Yield(YieldExpression {
                            key,
                            value: Some(value),
                        }),
                        Span::new(start_span.start, end_span.end),
                        CommentGroup::default(),
                    )
                }
            }
        }

        (TokenKind::Clone, _) => {
            let start_span = state.stream.current().span;
            state.stream.next();

            let target = for_precedence(state, Precedence::CloneOrNew);

            let end_span = state.stream.previous().span;

            Expression::new(
                ExpressionKind::Clone(CloneExpression {
                    target: Box::new(target),
                }),
                Span::new(start_span.start, end_span.end),
                CommentGroup::default(),
            )
        }

        (TokenKind::True, _) => {
            let span = state.stream.current().span;
            state.stream.next();

            Expression::new(
                ExpressionKind::Bool(BoolExpression { value: true }),
                span,
                CommentGroup::default(),
            )
        }

        (TokenKind::False, _) => {
            let span = state.stream.current().span;
            state.stream.next();

            Expression::new(
                ExpressionKind::Bool(BoolExpression { value: false }),
                span,
                CommentGroup::default(),
            )
        }

        (TokenKind::Null, _) => {
            let span = state.stream.current().span;
            state.stream.next();

            Expression::new(ExpressionKind::Null, span, CommentGroup::default())
        }

        (TokenKind::LiteralInteger, _) => {
            let span = state.stream.current().span;
            let current = state.stream.current();

            if let TokenKind::LiteralInteger = &current.kind {
                state.stream.next();

                Expression::new(
                    ExpressionKind::Literal(Literal::new(LiteralKind::Integer, *current)),
                    span,
                    CommentGroup::default(),
                )
            } else {
                unreachable!("{}:{}", file!(), line!());
            }
        }

        (TokenKind::LiteralFloat, _) => {
            let span = state.stream.current().span;
            let current = state.stream.current();

            if let TokenKind::LiteralFloat = &current.kind {
                state.stream.next();

                Expression::new(
                    ExpressionKind::Literal(Literal::new(LiteralKind::Float, *current)),
                    span,
                    CommentGroup::default(),
                )
            } else {
                unreachable!("{}:{}", file!(), line!());
            }
        }

        (TokenKind::LiteralSingleQuotedString | TokenKind::LiteralDoubleQuotedString, _) => {
            let span = state.stream.current().span;
            let current = state.stream.current();

            if let TokenKind::LiteralSingleQuotedString = &current.kind {
                state.stream.next();

                Expression::new(
                    ExpressionKind::Literal(Literal::new(LiteralKind::String, *current)),
                    span,
                    CommentGroup::default(),
                )
            } else if let TokenKind::LiteralDoubleQuotedString = &current.kind {
                state.stream.next();

                Expression::new(
                    ExpressionKind::Literal(Literal::new(LiteralKind::String, *current)),
                    span,
                    CommentGroup::default(),
                )
            } else {
                unreachable!("{}:{}", file!(), line!());
            }
        }

        (TokenKind::StringPart, _) => strings::interpolated(state),

        (TokenKind::StartHeredoc, _) => strings::heredoc(state),

        (TokenKind::StartNowdoc, _) => strings::nowdoc(state),

        (TokenKind::Backtick, _) => strings::shell_exec(state),

        (
            TokenKind::Identifier
            | TokenKind::QualifiedIdentifier
            | TokenKind::FullyQualifiedIdentifier,
            _,
        ) => {
            let name = names::full_name(
                state,
                match state.stream.peek().kind {
                    TokenKind::LeftParen => UseKind::Function,
                    TokenKind::DoubleColon => UseKind::Normal,
                    _ => UseKind::Const,
                },
            );

            Expression::new(
                ExpressionKind::Name(name),
                name.span,
                CommentGroup::default(),
            )
        }

        (TokenKind::Static, _) => {
            let span = state.stream.current().span;
            state.stream.next();
            let expression = Expression::new(ExpressionKind::Static, span, CommentGroup::default());

            postfix(state, expression, &TokenKind::DoubleColon)
        }

        (TokenKind::Self_, _) => {
            let span = state.stream.current().span;
            state.stream.next();

            Expression::new(ExpressionKind::Self_, span, CommentGroup::default())
        }

        (TokenKind::Parent, _) => {
            let span = state.stream.current().span;
            state.stream.next();

            Expression::new(ExpressionKind::Parent, span, CommentGroup::default())
        }

        (TokenKind::LeftParen, _) => {
            let start = state.stream.current().span;
            state.stream.next();

            let expr = create(state);

            let end = utils::skip_right_parenthesis(state);

            Expression::new(
                ExpressionKind::Parenthesized(ParenthesizedExpression {
                    start,
                    expr: Box::new(expr),
                    end,
                }),
                Span::new(start.start, end.end),
                CommentGroup::default(),
            )
        }

        (TokenKind::Match, _) => control_flow::match_expression(state),

        (TokenKind::Array, _) => arrays::array_expression(state),

        (TokenKind::LeftBracket, _) => arrays::short_array_expression(state),

        (TokenKind::New, _) => {
            let new = state.stream.current().span;

            state.stream.next();

            if state.stream.current().kind == TokenKind::Class
                || state.stream.current().kind == TokenKind::Attribute
            {
                return classes::parse_anonymous(state, Some(new));
            };

            let current_span = state.stream.current().span;
            let target = match state.stream.current().kind {
                TokenKind::Self_ => {
                    let token = state.stream.current();

                    state.stream.next();

                    Expression::new(
                        ExpressionKind::Name(Name::special(
                            SpecialNameKind::Self_,
                            token.symbol.unwrap(),
                            token.span,
                        )),
                        token.span,
                        CommentGroup::default(),
                    )
                }
                TokenKind::Static => {
                    let token = state.stream.current();

                    state.stream.next();

                    Expression::new(
                        ExpressionKind::Name(Name::special(
                            SpecialNameKind::Static,
                            token.symbol.unwrap(),
                            token.span,
                        )),
                        token.span,
                        CommentGroup::default(),
                    )
                }
                TokenKind::Parent => {
                    let token = state.stream.current();

                    state.stream.next();

                    Expression::new(
                        ExpressionKind::Name(Name::special(
                            SpecialNameKind::Parent,
                            token.symbol.unwrap(),
                            token.span,
                        )),
                        token.span,
                        CommentGroup::default(),
                    )
                }
                TokenKind::FullyQualifiedIdentifier => {
                    let token = state.stream.current();

                    let span = token.span;
                    let symbol = token.symbol.unwrap();
                    let resolved = state.strip_leading_namespace_qualifier(symbol);

                    state.stream.next();

                    Expression::new(
                        ExpressionKind::Name(Name::resolved(resolved, symbol, span)),
                        span,
                        CommentGroup::default(),
                    )
                }
                TokenKind::Identifier
                | TokenKind::QualifiedIdentifier
                | TokenKind::Enum
                | TokenKind::From => {
                    let token = state.stream.current();

                    state.stream.next();

                    Expression::new(
                        ExpressionKind::Name(
                            state.maybe_resolve_identifier(*token, UseKind::Normal),
                        ),
                        token.span,
                        CommentGroup::default(),
                    )
                }
                _ => clone_or_new_precedence(state),
            };

            let arguments = if state.stream.current().kind == TokenKind::LeftParen {
                Some(parameters::argument_list(state))
            } else {
                None
            };

            Expression::new(
                ExpressionKind::New(NewExpression {
                    target: Box::new(target),
                    new,
                    arguments,
                }),
                Span::new(new.start, current_span.end),
                CommentGroup::default(),
            )
        }

        (TokenKind::DirConstant, _) => {
            let span = state.stream.current().span;
            state.stream.next();

            Expression::new(
                ExpressionKind::MagicConstant(MagicConstantExpression::Directory(span)),
                span,
                CommentGroup::default(),
            )
        }

        (TokenKind::FileConstant, _) => {
            let span = state.stream.current().span;
            state.stream.next();

            Expression::new(
                ExpressionKind::MagicConstant(MagicConstantExpression::File(span)),
                span,
                CommentGroup::default(),
            )
        }

        (TokenKind::LineConstant, _) => {
            let span = state.stream.current().span;
            state.stream.next();

            Expression::new(
                ExpressionKind::MagicConstant(MagicConstantExpression::Line(span)),
                span,
                CommentGroup::default(),
            )
        }

        (TokenKind::FunctionConstant, _) => {
            let span = state.stream.current().span;
            state.stream.next();

            Expression::new(
                ExpressionKind::MagicConstant(MagicConstantExpression::Function(span)),
                span,
                CommentGroup::default(),
            )
        }

        (TokenKind::ClassConstant, _) => {
            let span = state.stream.current().span;
            state.stream.next();

            Expression::new(
                ExpressionKind::MagicConstant(MagicConstantExpression::Class(span)),
                span,
                CommentGroup::default(),
            )
        }

        (TokenKind::MethodConstant, _) => {
            let span = state.stream.current().span;
            state.stream.next();

            Expression::new(
                ExpressionKind::MagicConstant(MagicConstantExpression::Method(span)),
                span,
                CommentGroup::default(),
            )
        }

        (TokenKind::NamespaceConstant, _) => {
            let span = state.stream.current().span;
            state.stream.next();

            Expression::new(
                ExpressionKind::MagicConstant(MagicConstantExpression::Namespace(span)),
                span,
                CommentGroup::default(),
            )
        }

        (TokenKind::TraitConstant, _) => {
            let span = state.stream.current().span;
            state.stream.next();

            Expression::new(
                ExpressionKind::MagicConstant(MagicConstantExpression::Trait(span)),
                span,
                CommentGroup::default(),
            )
        }

        (TokenKind::CompilerHaltOffsetConstant, _) => {
            let span = state.stream.current().span;
            state.stream.next();

            Expression::new(
                ExpressionKind::MagicConstant(MagicConstantExpression::CompilerHaltOffset(span)),
                span,
                CommentGroup::default(),
            )
        }

        (
            TokenKind::Include
            | TokenKind::IncludeOnce
            | TokenKind::Require
            | TokenKind::RequireOnce,
            _,
        ) => {
            let start_span = state.stream.current().span;
            let current = state.stream.current();
            let span = current.span;

            state.stream.next();

            let path = Box::new(create(state));

            let kind = match current.kind {
                TokenKind::Include => ExpressionKind::Include(IncludeExpression {
                    include: span,
                    path,
                }),
                TokenKind::IncludeOnce => ExpressionKind::IncludeOnce(IncludeOnceExpression {
                    include_once: span,
                    path,
                }),
                TokenKind::Require => ExpressionKind::Require(RequireExpression {
                    require: span,
                    path,
                }),
                TokenKind::RequireOnce => ExpressionKind::RequireOnce(RequireOnceExpression {
                    require_once: span,
                    path,
                }),
                _ => unreachable!(),
            };

            let end_span = state.stream.previous().span;

            Expression::new(
                kind,
                Span::new(start_span.start, end_span.end),
                CommentGroup::default(),
            )
        }

        (
            TokenKind::StringCast
            | TokenKind::BinaryCast
            | TokenKind::ObjectCast
            | TokenKind::BoolCast
            | TokenKind::BooleanCast
            | TokenKind::IntCast
            | TokenKind::IntegerCast
            | TokenKind::FloatCast
            | TokenKind::DoubleCast
            | TokenKind::RealCast
            | TokenKind::UnsetCast
            | TokenKind::ArrayCast,
            _,
        ) => {
            let current = state.stream.current();

            let span = current.span;
            let kind = current.kind.into();

            state.stream.next();

            let rhs = for_precedence(state, Precedence::Prefix);
            let rhs_span = rhs.span;

            Expression::new(
                ExpressionKind::Cast(CastExpression {
                    cast: span,
                    kind,
                    value: Box::new(rhs),
                }),
                Span::new(span.start, rhs_span.end),
                CommentGroup::default(),
            )
        }

        (TokenKind::Decrement | TokenKind::Increment | TokenKind::Minus | TokenKind::Plus, _) => {
            let start_span = state.stream.current().span;
            let current = state.stream.current();

            let span = current.span;
            let op = current.kind;

            state.stream.next();

            let right = Box::new(for_precedence(state, Precedence::Prefix));
            let right_span = right.span;
            let expr = match op {
                TokenKind::Minus => {
                    ExpressionKind::ArithmeticOperation(ArithmeticOperationKind::Negative {
                        minus: span,
                        right,
                    })
                }
                TokenKind::Plus => {
                    ExpressionKind::ArithmeticOperation(ArithmeticOperationKind::Positive {
                        plus: span,
                        right,
                    })
                }
                TokenKind::Decrement => {
                    ExpressionKind::ArithmeticOperation(ArithmeticOperationKind::PreDecrement {
                        decrement: span,
                        right,
                    })
                }
                TokenKind::Increment => {
                    ExpressionKind::ArithmeticOperation(ArithmeticOperationKind::PreIncrement {
                        increment: span,
                        right,
                    })
                }
                _ => unreachable!(),
            };

            Expression::new(
                expr,
                Span::new(start_span.start, right_span.end),
                CommentGroup::default(),
            )
        }

        (TokenKind::Bang, _) => {
            let start_span = state.stream.current().span;
            let bang = state.stream.current().span;

            state.stream.next();

            let rhs = for_precedence(state, Precedence::Bang);
            let end_span = rhs.span;

            Expression::new(
                ExpressionKind::LogicalOperation(LogicalOperationKind::Not {
                    bang,
                    right: Box::new(rhs),
                }),
                Span::new(start_span.start, end_span.end),
                CommentGroup::default(),
            )
        }

        (TokenKind::At, _) => {
            let span = state.stream.current().span;

            state.stream.next();

            let rhs = for_precedence(state, Precedence::Prefix);
            let end_span = rhs.span;

            Expression::new(
                ExpressionKind::ErrorSuppress(ErrorSuppressExpression {
                    at: span,
                    expr: Box::new(rhs),
                }),
                Span::new(span.start, end_span.end),
                CommentGroup::default(),
            )
        }

        (TokenKind::BitwiseNot, _) => {
            let span = state.stream.current().span;

            state.stream.next();

            let right = Box::new(for_precedence(state, Precedence::Prefix));
            let end_span = right.span;

            Expression::new(
                ExpressionKind::BitwiseOperation(BitwiseOperationKind::Not { not: span, right }),
                Span::new(span.start, end_span.end),
                CommentGroup::default(),
            )
        }

        (TokenKind::Dollar | TokenKind::DollarLeftBrace | TokenKind::Variable, _) => {
            let span = state.stream.current().span;

            Expression::new(
                ExpressionKind::Variable(variables::dynamic_variable(state)),
                span,
                CommentGroup::default(),
            )
        }

        _ => unexpected_token(state, precedence),
    }
}

fn unexpected_token(state: &mut State, _: &Precedence) -> Expression {
    let current = state.stream.current();

    state.diagnostic(
        ParserDiagnostic::UnexpectedToken { token: *current },
        Severity::Error,
        current.span,
    );

    // This is a common case where we don't want to consume the right-brace as it might close a structure.
    if current.kind != TokenKind::RightBrace {
        state.stream.next();
    }

    Expression::missing(current.span)
}

fn postfix(state: &mut State, lhs: Expression, op: &TokenKind) -> Expression {
    let start_span = state.stream.current().span;
    let kind = match op {
        TokenKind::DoubleQuestion => {
            let double_question = state.stream.current().span;
            state.stream.next();

            let rhs = null_coalesce_precedence(state);

            ExpressionKind::Coalesce(CoalesceExpression {
                lhs: Box::new(lhs),
                double_question,
                rhs: Box::new(rhs),
            })
        }
        TokenKind::LeftParen => {
            // `(...)` closure creation
            if state.stream.lookahead(0).kind == TokenKind::Ellipsis
                && state.stream.lookahead(1).kind == TokenKind::RightParen
            {
                let start = utils::skip(state, TokenKind::LeftParen);
                let ellipsis = utils::skip(state, TokenKind::Ellipsis);
                let end = utils::skip(state, TokenKind::RightParen);

                let placeholder = ArgumentPlaceholder {
                    comments: state.stream.comments(),
                    left_parenthesis: start,
                    ellipsis,
                    right_parenthesis: end,
                };

                ExpressionKind::FunctionClosureCreation(FunctionClosureCreationExpression {
                    target: Box::new(lhs),
                    placeholder,
                })
            } else {
                let arguments = parameters::argument_list(state);

                ExpressionKind::FunctionCall(FunctionCallExpression {
                    target: Box::new(lhs),
                    arguments,
                })
            }
        }
        TokenKind::LeftBracket => ExpressionKind::ArrayIndex(ArrayIndexExpression {
            array: Box::new(lhs),
            left_bracket: utils::skip_left_bracket(state),
            index: if state.stream.current().kind == TokenKind::RightBracket {
                None
            } else {
                Some(Box::new(create(state)))
            },
            right_bracket: utils::skip_right_bracket(state),
        }),
        TokenKind::DoubleColon => {
            let span = utils::skip_double_colon(state);

            let current = state.stream.current();

            let property = match current.kind {
                TokenKind::Variable | TokenKind::Dollar | TokenKind::DollarLeftBrace => {
                    ExpressionKind::Variable(variables::dynamic_variable(state))
                }
                _ if identifiers::is_identifier_maybe_reserved(&state.stream.current().kind) => {
                    ExpressionKind::Identifier(Identifier::SimpleIdentifier(
                        identifiers::identifier_maybe_reserved(state),
                    ))
                }
                TokenKind::LeftBrace => {
                    let start = current.span;

                    state.stream.next();

                    let expr = Box::new(create(state));
                    let end = utils::skip_right_brace(state);

                    let span = Span::new(start.start, end.end);

                    ExpressionKind::Identifier(Identifier::DynamicIdentifier(DynamicIdentifier {
                        span,
                        expr,
                    }))
                }
                TokenKind::Class => {
                    state.stream.next();

                    let symbol = current.symbol.unwrap();

                    ExpressionKind::Identifier(Identifier::SimpleIdentifier(SimpleIdentifier::new(
                        symbol,
                        current.span,
                    )))
                }
                _ => {
                    state.diagnostic(
                        ParserDiagnostic::ExpectedToken {
                            expected: vec![
                                TokenKind::LeftBrace,
                                TokenKind::Dollar,
                                TokenKind::Identifier,
                            ],
                            found: *current,
                        },
                        Severity::Error,
                        current.span,
                    );

                    state.stream.next();

                    ExpressionKind::Missing
                }
            };

            let lhs = Box::new(lhs);

            if state.stream.current().kind == TokenKind::LeftParen {
                if state.stream.lookahead(0).kind == TokenKind::Ellipsis
                    && state.stream.lookahead(1).kind == TokenKind::RightParen
                {
                    let start = utils::skip(state, TokenKind::LeftParen);
                    let ellipsis = utils::skip(state, TokenKind::Ellipsis);
                    let end = utils::skip(state, TokenKind::RightParen);

                    let placeholder = ArgumentPlaceholder {
                        comments: state.stream.comments(),
                        left_parenthesis: start,
                        ellipsis,
                        right_parenthesis: end,
                    };

                    match property {
                        ExpressionKind::Identifier(identifier) => {
                            ExpressionKind::StaticMethodClosureCreation(
                                StaticMethodClosureCreationExpression {
                                    target: lhs,
                                    double_colon: span,
                                    method: identifier,
                                    placeholder,
                                },
                            )
                        }
                        ExpressionKind::Variable(variable) => {
                            ExpressionKind::StaticVariableMethodClosureCreation(
                                StaticVariableMethodClosureCreationExpression {
                                    target: lhs,
                                    double_colon: span,
                                    method: variable,
                                    placeholder,
                                },
                            )
                        }
                        _ => unreachable!(),
                    }
                } else {
                    let arguments = parameters::argument_list(state);

                    match property {
                        ExpressionKind::Identifier(identifier) => {
                            ExpressionKind::StaticMethodCall(StaticMethodCallExpression {
                                target: lhs,
                                double_colon: span,
                                method: identifier,
                                arguments,
                            })
                        }
                        ExpressionKind::Variable(variable) => {
                            ExpressionKind::StaticVariableMethodCall(
                                StaticVariableMethodCallExpression {
                                    target: lhs,
                                    double_colon: span,
                                    method: variable,
                                    arguments,
                                },
                            )
                        }
                        _ => unreachable!(),
                    }
                }
            } else {
                match property {
                    ExpressionKind::Identifier(identifier) => {
                        ExpressionKind::ConstantFetch(ConstantFetchExpression {
                            target: lhs,
                            double_colon: span,
                            constant: identifier,
                        })
                    }
                    ExpressionKind::Variable(variable) => {
                        ExpressionKind::StaticPropertyFetch(StaticPropertyFetchExpression {
                            target: lhs,
                            double_colon: span,
                            property: variable,
                        })
                    }
                    _ => unreachable!(),
                }
            }
        }
        TokenKind::Arrow | TokenKind::QuestionArrow => {
            let span = state.stream.current().span;
            state.stream.next();

            let property = match state.stream.current().kind {
                TokenKind::Variable | TokenKind::Dollar | TokenKind::DollarLeftBrace => {
                    let start_span = state.stream.current().span;
                    let kind = ExpressionKind::Variable(variables::dynamic_variable(state));
                    let end_span = state.stream.previous().span;

                    Expression::new(
                        kind,
                        Span::new(start_span.start, end_span.end),
                        CommentGroup::default(),
                    )
                }
                _ if identifiers::is_identifier_maybe_reserved(&state.stream.current().kind) => {
                    let start_span = state.stream.current().span;
                    let kind = ExpressionKind::Identifier(Identifier::SimpleIdentifier(
                        identifiers::identifier_maybe_reserved(state),
                    ));
                    let end_span = state.stream.previous().span;

                    Expression::new(
                        kind,
                        Span::new(start_span.start, end_span.end),
                        CommentGroup::default(),
                    )
                }
                TokenKind::LeftBrace => {
                    let start = state.stream.current().span;
                    state.stream.next();

                    let name = create(state);

                    let end = utils::skip_right_brace(state);
                    let span = Span::new(start.start, end.end);

                    Expression::new(
                        ExpressionKind::Identifier(Identifier::DynamicIdentifier(
                            DynamicIdentifier {
                                span,
                                expr: Box::new(name),
                            },
                        )),
                        Span::new(start.start, end.end),
                        CommentGroup::default(),
                    )
                }
                _ => {
                    let span = state.stream.current().span;

                    state.diagnostic(
                        ParserDiagnostic::ExpectedToken {
                            expected: vec![
                                TokenKind::LeftBrace,
                                TokenKind::Dollar,
                                TokenKind::Identifier,
                            ],
                            found: *state.stream.current(),
                        },
                        Severity::Error,
                        span,
                    );

                    state.stream.next();

                    Expression::missing(span)
                }
            };

            if state.stream.current().kind == TokenKind::LeftParen {
                if op == &TokenKind::QuestionArrow {
                    let arguments = parameters::argument_list(state);

                    ExpressionKind::NullsafeMethodCall(NullsafeMethodCallExpression {
                        target: Box::new(lhs),
                        method: Box::new(property),
                        question_arrow: span,
                        arguments,
                    })
                } else {
                    // `(...)` closure creation
                    if state.stream.lookahead(0).kind == TokenKind::Ellipsis
                        && state.stream.lookahead(1).kind == TokenKind::RightParen
                    {
                        let start = utils::skip(state, TokenKind::LeftParen);
                        let ellipsis = utils::skip(state, TokenKind::Ellipsis);
                        let end = utils::skip(state, TokenKind::RightParen);

                        let placeholder = ArgumentPlaceholder {
                            comments: state.stream.comments(),
                            left_parenthesis: start,
                            ellipsis,
                            right_parenthesis: end,
                        };

                        ExpressionKind::MethodClosureCreation(MethodClosureCreationExpression {
                            target: Box::new(lhs),
                            method: Box::new(property),
                            arrow: span,
                            placeholder,
                        })
                    } else {
                        let arguments = parameters::argument_list(state);

                        ExpressionKind::MethodCall(MethodCallExpression {
                            target: Box::new(lhs),
                            method: Box::new(property),
                            arrow: span,
                            arguments,
                        })
                    }
                }
            } else if op == &TokenKind::QuestionArrow {
                ExpressionKind::NullsafePropertyFetch(NullsafePropertyFetchExpression {
                    target: Box::new(lhs),
                    question_arrow: span,
                    property: Box::new(property),
                })
            } else {
                ExpressionKind::PropertyFetch(PropertyFetchExpression {
                    target: Box::new(lhs),
                    arrow: span,
                    property: Box::new(property),
                })
            }
        }
        TokenKind::Increment => {
            let span = state.stream.current().span;
            state.stream.next();

            ExpressionKind::ArithmeticOperation(ArithmeticOperationKind::PostIncrement {
                left: Box::new(lhs),
                increment: span,
            })
        }
        TokenKind::Decrement => {
            let span = state.stream.current().span;
            state.stream.next();

            ExpressionKind::ArithmeticOperation(ArithmeticOperationKind::PostDecrement {
                left: Box::new(lhs),
                decrement: span,
            })
        }
        _ => unreachable!(),
    };

    let end_span = state.stream.previous().span;

    Expression::new(
        kind,
        Span::new(start_span.start, end_span.end),
        CommentGroup::default(),
    )
}

fn is_infix(t: &TokenKind) -> bool {
    matches!(
        t,
        TokenKind::Pow
            | TokenKind::RightShiftEquals
            | TokenKind::LeftShiftEquals
            | TokenKind::CaretEquals
            | TokenKind::AmpersandEquals
            | TokenKind::PipeEquals
            | TokenKind::PercentEquals
            | TokenKind::PowEquals
            | TokenKind::LogicalAnd
            | TokenKind::LogicalOr
            | TokenKind::LogicalXor
            | TokenKind::Spaceship
            | TokenKind::LeftShift
            | TokenKind::RightShift
            | TokenKind::Ampersand
            | TokenKind::Pipe
            | TokenKind::Caret
            | TokenKind::Percent
            | TokenKind::Instanceof
            | TokenKind::Asterisk
            | TokenKind::Slash
            | TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Dot
            | TokenKind::LessThan
            | TokenKind::GreaterThan
            | TokenKind::LessThanEquals
            | TokenKind::GreaterThanEquals
            | TokenKind::DoubleEquals
            | TokenKind::TripleEquals
            | TokenKind::BangEquals
            | TokenKind::BangDoubleEquals
            | TokenKind::AngledLeftRight
            | TokenKind::Question
            | TokenKind::QuestionColon
            | TokenKind::BooleanAnd
            | TokenKind::BooleanOr
            | TokenKind::Equals
            | TokenKind::PlusEquals
            | TokenKind::MinusEquals
            | TokenKind::DotEquals
            | TokenKind::DoubleQuestionEquals
            | TokenKind::AsteriskEquals
            | TokenKind::SlashEquals
    )
}

#[inline(always)]
fn is_postfix(t: &TokenKind) -> bool {
    matches!(
        t,
        TokenKind::Increment
            | TokenKind::Decrement
            | TokenKind::LeftParen
            | TokenKind::LeftBracket
            | TokenKind::Arrow
            | TokenKind::QuestionArrow
            | TokenKind::DoubleColon
            | TokenKind::DoubleQuestion
    )
}
