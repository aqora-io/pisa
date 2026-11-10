//! Renders Beam's structured goals as Lean prints a goal, so they can be
//! compared with the REPL's goal strings.

use crate::tools::Goal;

/// `case` line, one line per hypothesis group (`a b : Nat`, `x : T := v`),
/// then the target after its prefix (`⊢ `).
pub fn render(goal: &Goal) -> String {
    let mut out = String::new();
    if let Some(case) = goal.user_name.as_deref().filter(|n| !n.is_empty()) {
        out.push_str("case ");
        out.push_str(case);
        out.push('\n');
    }
    for hyp in &goal.hyps {
        out.push_str(&hyp.names.join(" "));
        out.push_str(" : ");
        out.push_str(&hyp.ty);
        if let Some(value) = &hyp.value {
            out.push_str(" := ");
            out.push_str(value);
        }
        out.push('\n');
    }
    out.push_str(if goal.goal_prefix.is_empty() {
        "⊢ "
    } else {
        &goal.goal_prefix
    });
    out.push_str(&goal.target);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Hyp;

    #[test]
    fn renders_like_lean() {
        let goal = Goal {
            user_name: Some("succ".into()),
            goal_prefix: "⊢ ".into(),
            hyps: vec![
                Hyp {
                    names: vec!["a".into(), "b".into()],
                    ty: "Nat".into(),
                    value: None,
                },
                Hyp {
                    names: vec!["k".into()],
                    ty: "Nat".into(),
                    value: Some("a + b".into()),
                },
            ],
            target: "k = b + a".into(),
        };
        assert_eq!(
            render(&goal),
            "case succ\na b : Nat\nk : Nat := a + b\n⊢ k = b + a"
        );
        let bare = Goal {
            user_name: None,
            goal_prefix: String::new(),
            hyps: vec![],
            target: "True".into(),
        };
        assert_eq!(render(&bare), "⊢ True");
    }
}
