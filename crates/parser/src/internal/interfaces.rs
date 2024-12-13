use crate::internal::utils;
use crate::state::State;
use crate::Parser;
use pxp_ast::StatementKind;
use pxp_ast::UseKind;
use pxp_ast::*;
use pxp_span::Span;
use pxp_span::Spanned;
use pxp_token::TokenKind;

use super::classes::parse_classish_member;
use super::names;

impl<'a> Parser<'a> {
    pub fn parse_interface(&mut self) -> StatementKind {
        let span = self.skip(TokenKind::Interface);

        let name = names::parse_type_name();

        let current = self.current();
        let extends = if current.kind == TokenKind::Extends {
            let span = current.span;

            self.next();

            let parents =
                utils::at_least_one_comma_separated_no_trailing::<Name>(state, &|state| {
                    names::parse_full_name(state, UseKind::Normal)
                });

            Some(InterfaceExtends {
                id: self.state.id(),
                span: Span::combine(span, parents.span()),
                extends: span,
                parents,
            })
        } else {
            None
        };

        let attributes = state.get_attributes();

        let left_brace = utils::skip_left_brace();
        let members = {
            let mut members = Vec::new();
            while self.current().kind != TokenKind::RightBrace {
                members.push(parse_classish_member(state, true));
            }

            members
        };
        let right_brace = utils::skip_right_brace();

        let body = InterfaceBody {
            id: self.state.id(),
            span: Span::combine(left_brace, right_brace),
            left_brace,
            members,
            right_brace,
        };

        StatementKind::Interface(InterfaceStatement {
            id: self.state.id(),
            span: Span::combine(span, body.span),
            interface: span,
            name,
            attributes,
            extends,
            body,
        })
    }
}
