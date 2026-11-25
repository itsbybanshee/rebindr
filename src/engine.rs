//! Turns the set of held controller buttons (a bitmask) into shortcut presses.
//!
//! * A mapping fires once when all its buttons are held, and re-arms on release.
//! * Bigger combos win: if View+A fires, a mapping on View alone does not.
//! * A mapping whose buttons are part of a bigger combo is "deferred": it fires when
//!   released, or once held for DEFER, unless a combo fires first.
//! * A long-press mapping fires after LONG. A normal mapping on the same buttons then
//!   fires on release only, so a long press never also triggers it.
//! * Hold mode keeps the keys down for as long as the buttons are held.

use std::time::{Duration, Instant};

use crate::keys::Phase;

pub const DEFER: Duration = Duration::from_millis(300);
pub const LONG: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    Keys(Vec<u16>),
    Open(String),
}

pub struct Rule {
    pub mask: u32,
    pub act: Act,
    pub hold: bool,
    pub long: bool,
}

struct Entry {
    rule: Rule,
    deferred: bool,
    release_only: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum St {
    Idle,
    Fired,
    Held,
    Cancelled,
    Pending(Instant),
}

pub struct Engine {
    maps: Vec<Entry>,
    st: Vec<St>,
    order: Vec<usize>, // biggest combos first
}

impl Engine {
    pub fn new(rules: Vec<Rule>) -> Self {
        let masks: Vec<(u32, bool)> = rules.iter().map(|r| (r.mask, r.long)).collect();
        let maps: Vec<Entry> = rules
            .into_iter()
            .map(|r| {
                let deferred = masks.iter().any(|&(o, _)| o != r.mask && o & r.mask == r.mask);
                let release_only = !r.long && masks.iter().any(|&(o, l)| l && o == r.mask);
                Entry { rule: r, deferred, release_only }
            })
            .collect();
        let mut order: Vec<usize> = (0..maps.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(maps[i].rule.mask.count_ones()));
        let n = maps.len();
        Engine { maps, st: vec![St::Idle; n], order }
    }

    pub fn reset(&mut self, out: &mut dyn FnMut(&Act, Phase)) {
        for i in 0..self.maps.len() {
            if self.st[i] == St::Held {
                out(&self.maps[i].rule.act, Phase::Up);
            }
            self.st[i] = St::Idle;
        }
    }

    fn activate(&mut self, i: usize, out: &mut dyn FnMut(&Act, Phase)) {
        let r = &self.maps[i].rule;
        if r.hold && matches!(r.act, Act::Keys(_)) {
            out(&r.act, Phase::Down);
            self.st[i] = St::Held;
        } else {
            out(&r.act, Phase::Tap);
            self.st[i] = St::Fired;
        }
        let m = r.mask;
        for j in 0..self.maps.len() {
            if j != i && matches!(self.st[j], St::Pending(_)) && self.maps[j].rule.mask & m != 0 {
                self.st[j] = St::Cancelled;
            }
        }
    }

    pub fn update(&mut self, held: u32, now: Instant, out: &mut dyn FnMut(&Act, Phase)) {
        // 1. Resolve mappings whose buttons are no longer all held.
        for i in 0..self.maps.len() {
            let m = self.maps[i].rule.mask;
            if self.st[i] == St::Idle || held & m == m {
                continue;
            }
            match self.st[i] {
                St::Held => out(&self.maps[i].rule.act, Phase::Up),
                St::Pending(_) if !self.maps[i].rule.long => out(&self.maps[i].rule.act, Phase::Tap),
                _ => {} // a long press released too early simply doesn't fire
            }
            self.st[i] = St::Idle;
        }

        let mut consumed = 0u32;
        for i in 0..self.maps.len() {
            if matches!(self.st[i], St::Fired | St::Held) {
                consumed |= self.maps[i].rule.mask;
            }
        }

        // 2. Pending presses held long enough -> activate.
        for k in 0..self.order.len() {
            let i = self.order[k];
            let e = &self.maps[i];
            if let St::Pending(t0) = self.st[i] {
                if e.rule.mask & consumed != 0 || e.release_only {
                    continue;
                }
                if now.duration_since(t0) >= if e.rule.long { LONG } else { DEFER } {
                    let m = e.rule.mask;
                    self.activate(i, out);
                    consumed |= m;
                }
            }
        }

        // 3. Newly satisfied mappings, biggest combos first.
        for k in 0..self.order.len() {
            let i = self.order[k];
            let e = &self.maps[i];
            let m = e.rule.mask;
            if self.st[i] != St::Idle || held & m != m || m & consumed != 0 {
                continue;
            }
            if e.deferred || e.rule.long || e.release_only {
                self.st[i] = St::Pending(now);
                continue;
            }
            self.activate(i, out);
            consumed |= m;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buttons::SHARE_BIT;

    const A: u32 = 1 << 14;
    const B: u32 = 1 << 15;
    const VIEW: u32 = 1;
    const RB: u32 = 1 << 5;
    const RT: u32 = 1 << 7;
    const LS: u32 = 1 << 8;

    fn rule(mask: u32, key: u16) -> Rule {
        Rule { mask, act: Act::Keys(vec![key]), hold: false, long: false }
    }

    fn run(e: &mut Engine, steps: &[(u32, u64)]) -> Vec<(u16, Phase)> {
        let t0 = Instant::now();
        let mut calls = Vec::new();
        for &(held, ms) in steps {
            e.update(held, t0 + Duration::from_millis(ms), &mut |a, p| {
                if let Act::Keys(k) = a {
                    calls.push((k[0], p))
                }
            });
        }
        calls
    }

    #[test]
    fn single_fires_once_and_rearms() {
        let mut e = Engine::new(vec![rule(A, 1)]);
        assert_eq!(run(&mut e, &[(A, 0), (A, 500), (0, 600), (A, 700)]), [(1, Phase::Tap), (1, Phase::Tap)]);
    }

    #[test]
    fn combo_beats_single() {
        let mut e = Engine::new(vec![rule(VIEW, 1), rule(VIEW | A, 2)]);
        assert_eq!(run(&mut e, &[(VIEW, 0), (VIEW | A, 100), (VIEW, 200), (0, 300)]), [(2, Phase::Tap)]);
    }

    #[test]
    fn deferred_single_quick_release_and_after_hold() {
        let mut e = Engine::new(vec![rule(VIEW, 1), rule(VIEW | A, 2)]);
        assert_eq!(run(&mut e, &[(VIEW, 0), (0, 100)]), [(1, Phase::Tap)]);
        let mut e = Engine::new(vec![rule(VIEW, 1), rule(VIEW | A, 2)]);
        assert_eq!(run(&mut e, &[(VIEW, 0), (VIEW, 350), (VIEW | A, 400), (0, 500)]), [(1, Phase::Tap)]);
    }

    #[test]
    fn modifier_chaining() {
        let mut e = Engine::new(vec![rule(VIEW | A, 1), rule(VIEW | B, 2)]);
        assert_eq!(run(&mut e, &[(VIEW, 0), (VIEW | A, 100), (VIEW, 200), (VIEW | B, 300)]), [(1, Phase::Tap), (2, Phase::Tap)]);
    }

    #[test]
    fn hold_mode_and_reset() {
        let mut r = rule(RB, 9);
        r.hold = true;
        let mut e = Engine::new(vec![r]);
        assert_eq!(run(&mut e, &[(RB, 0), (RB, 500), (0, 600)]), [(9, Phase::Down), (9, Phase::Up)]);
        let mut calls = run(&mut e, &[(RB, 1000)]);
        e.reset(&mut |a, p| if let Act::Keys(k) = a { calls.push((k[0], p)) });
        assert_eq!(calls, [(9, Phase::Down), (9, Phase::Up)]);
    }

    #[test]
    fn extra_held_buttons_dont_block() {
        let mut e = Engine::new(vec![rule(VIEW | A, 1)]);
        assert_eq!(run(&mut e, &[(RT | LS, 0), (RT | LS | VIEW | A, 100)]), [(1, Phase::Tap)]);
    }

    #[test]
    fn long_press_alone() {
        let mut r = rule(SHARE_BIT, 7);
        r.long = true;
        let mut e = Engine::new(vec![r]);
        assert_eq!(run(&mut e, &[(SHARE_BIT, 0), (0, 200)]), []);
        assert_eq!(
            run(&mut e, &[(SHARE_BIT, 1000), (SHARE_BIT, 1300), (SHARE_BIT, 1550), (SHARE_BIT, 2500), (0, 2600)]),
            [(7, Phase::Tap)]
        );
    }

    #[test]
    fn press_and_long_press_on_same_button() {
        let mut l = rule(SHARE_BIT, 2);
        l.long = true;
        let mut e = Engine::new(vec![rule(SHARE_BIT, 1), l]);
        assert_eq!(run(&mut e, &[(SHARE_BIT, 0), (SHARE_BIT, 150), (0, 200)]), [(1, Phase::Tap)]);
        assert_eq!(run(&mut e, &[(SHARE_BIT, 1000), (SHARE_BIT, 1400), (SHARE_BIT, 1600), (0, 2500)]), [(2, Phase::Tap)]);
    }

    #[test]
    fn long_press_cancelled_by_combo() {
        let mut l = rule(SHARE_BIT, 1);
        l.long = true;
        let mut e = Engine::new(vec![l, rule(SHARE_BIT | VIEW, 2)]);
        assert_eq!(run(&mut e, &[(SHARE_BIT, 0), (SHARE_BIT | VIEW, 200), (SHARE_BIT | VIEW, 900), (0, 1000)]), [(2, Phase::Tap)]);
    }
}
