//! The job deadline budgets for its own shutdown.
//!
//! `arm_job_deadline` is process-wide and one-shot, so this is its own test binary: a test that
//! arms the process clock cannot share one with tests that assume no budget was ever armed
//! (`batch::schedule`'s `no_armed_deadline_means_no_budget`, for one). Everything here is one
//! `#[test]` for the same reason — the arming and the reserve are each one-shot, and two tests
//! racing for them would make the order decide the result.
//!
//! What is under test is the arithmetic, not a router: `Issue209-split10` measured 11.4 s of
//! work past its `--job-timeout` and was `SIGKILL`ed at the bench cap holding a routed board it
//! had not written. The fix is that the deadline everything polls
//! ([`fr_router::JobDeadline::ambient`]) retreats by what the shutdown is expected to cost.

use std::time::Duration;

use fr_router::{arm_job_deadline, job_budget_spent, job_tail_reserve, reserve_job_tail};

#[test]
fn the_reserve_moves_the_work_deadline_off_the_hard_one() {
    // Nothing armed yet: there is no budget to carve a reserve out of, and asking for one is a
    // no-op rather than an error — a library caller may reserve unconditionally.
    assert_eq!(reserve_job_tail(Duration::from_secs(1)), Duration::ZERO);
    assert_eq!(job_tail_reserve(), Duration::ZERO);
    assert!(!job_budget_spent(), "no deadline, no expiry");

    const BUDGET: Duration = Duration::from_secs(100);
    assert!(arm_job_deadline(BUDGET), "the first arming wins");
    let hard = fr_router::JobDeadline::ambient_hard();
    assert!(
        !arm_job_deadline(Duration::from_secs(1)),
        "a second arming is refused, and must not shorten the first"
    );

    let remaining_before = fr_router::JobDeadline::ambient()
        .remaining()
        .expect("a budget is armed");
    assert!(
        remaining_before > BUDGET - Duration::from_secs(1),
        "no reserve yet, so the work deadline is the hard one: {remaining_before:?}"
    );

    // 4 s of a 100 s budget is well under the quarter cap, so it is taken whole.
    assert_eq!(
        reserve_job_tail(Duration::from_secs(4)),
        Duration::from_secs(4)
    );
    assert_eq!(job_tail_reserve(), Duration::from_secs(4));

    let work = fr_router::JobDeadline::ambient()
        .remaining()
        .expect("still armed");
    let hard_left = hard.remaining().expect("still armed");
    let gap = hard_left.saturating_sub(work);
    assert!(
        gap >= Duration::from_millis(3_900) && gap <= Duration::from_millis(4_100),
        "the work deadline sits one reserve before the hard one: work {work:?}, hard {hard_left:?}"
    );
    assert!(
        !job_budget_spent(),
        "96 s of a 100 s budget is not 'spent': {work:?}"
    );

    // One-shot: a second caller gets the reserve already in force, and cannot move a deadline
    // the router is already stopping against.
    assert_eq!(
        reserve_job_tail(Duration::from_secs(30)),
        Duration::from_secs(4)
    );
    assert_eq!(job_tail_reserve(), Duration::from_secs(4));
}
