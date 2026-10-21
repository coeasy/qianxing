//! Multi-leg attribution is implemented once in `qx-app`; this module keeps the CLI facade thin.

pub(crate) use qx_app::{
    multi_leg_integrity_summary, rejection_count, rejection_facts_line, spread_group_id,
};
