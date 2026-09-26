//! The backup cycle collector. Values are reference counted, which frees
//! everything at once except cycles: a list inside itself, objects pointing
//! at each other. Those are found here by trial deletion, as CPython does:
//!
//! 1. every container (list, dictionary, object) is remembered weakly when
//!    it is made;
//! 2. a container's count minus the references other containers hold to it
//!    is what the rest of the program (VM registers, globals, natives) holds;
//! 3. what is reachable from containers with such outside references lives,
//!    and the rest can only be reached from itself: its contents are taken
//!    out, which frees it.
//!
//! A collection runs at a safe point (the VM making a container, when no
//! container is borrowed), once the containers made since the last one
//! outnumber both a floor and the survivors of the last one, so its cost
//! stays proportional to allocation. Programs never see it: Hana (Go's
//! collector) has no leaks either, and nothing runs when a value dies.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use haru_abi::tag;

use crate::value::{DictObj, Key, ListObj, ObjObj, Value};

enum Tracked {
    List(Weak<ListObj>),
    Dict(Weak<DictObj>),
    Obj(Weak<ObjObj>),
}

/// A live container during a collection (holding it alive meanwhile).
enum Live {
    List(Rc<ListObj>),
    Dict(Rc<DictObj>),
    Obj(Rc<ObjObj>),
}

impl Live {
    fn addr(&self) -> usize {
        match self {
            Live::List(r) => Rc::as_ptr(r) as usize,
            Live::Dict(r) => Rc::as_ptr(r) as usize,
            Live::Obj(r) => Rc::as_ptr(r) as usize,
        }
    }

    /// Its count, less the reference this collection holds.
    fn count(&self) -> isize {
        (match self {
            Live::List(r) => Rc::strong_count(r),
            Live::Dict(r) => Rc::strong_count(r),
            Live::Obj(r) => Rc::strong_count(r),
        }) as isize
            - 1
    }

    /// Calls `f` with each container it holds; false when it is borrowed now.
    fn children(&self, f: &mut dyn FnMut(usize)) -> bool {
        let mut visit = |v: &Value| {
            if matches!(v.tag(), tag::LIST | tag::DICT | tag::OBJECT) {
                f(v.raw().payload as usize);
            }
        };
        match self {
            Live::List(r) => match r.items.try_borrow() {
                Ok(items) => items.iter().for_each(&mut visit),
                Err(_) => return false,
            },
            Live::Dict(r) => match r.map.try_borrow() {
                Ok(map) => map.iter().for_each(|(k, v)| {
                    visit(&k.0);
                    visit(v);
                }),
                Err(_) => return false,
            },
            Live::Obj(r) => match r.props.try_borrow() {
                Ok(props) => props.values().for_each(&mut visit),
                Err(_) => return false,
            },
        }
        true
    }

    /// Takes its contents out (to be dropped once every garbage container is emptied).
    fn empty(&self, out: &mut Vec<Value>, keys: &mut Vec<Key>) {
        match self {
            Live::List(r) => out.extend(r.items.borrow_mut().take_all()),
            Live::Dict(r) => {
                for (k, v) in r.map.borrow_mut().drain() {
                    keys.push(k);
                    out.push(v);
                }
            }
            Live::Obj(r) => out.extend(r.props.borrow_mut().drain().map(|(_, v)| v)),
        }
    }
}

/// Fewest containers made between two collections.
const FLOOR: usize = 10_000;

thread_local! {
    static TRACKED: RefCell<Vec<Tracked>> = const { RefCell::new(Vec::new()) };
    /// Containers made since the last collection, and how many to wait for.
    static MADE: Cell<usize> = const { Cell::new(0) };
    static NEXT: Cell<usize> = const { Cell::new(FLOOR) };
    static STATS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}

pub(crate) fn track_list(r: &Rc<ListObj>) {
    track(Tracked::List(Rc::downgrade(r)));
}

pub(crate) fn track_dict(r: &Rc<DictObj>) {
    track(Tracked::Dict(Rc::downgrade(r)));
}

pub(crate) fn track_object(r: &Rc<ObjObj>) {
    track(Tracked::Obj(Rc::downgrade(r)));
}

fn track(t: Tracked) {
    TRACKED.with(|v| v.borrow_mut().push(t));
    MADE.with(|m| m.set(m.get() + 1));
}

/// Where the VM may collect: runs a collection when one is due
/// (`HARU_GC=0` turns collections off, to measure them).
#[inline]
pub fn safe_point() {
    if MADE.with(Cell::get) >= NEXT.with(Cell::get) {
        if std::env::var_os("HARU_GC").is_some_and(|v| v == "0") {
            // Off: keep tracking bounded all the same.
            TRACKED.with(|t| t.borrow_mut().retain(|x| match x {
                Tracked::List(w) => w.strong_count() > 0,
                Tracked::Dict(w) => w.strong_count() > 0,
                Tracked::Obj(w) => w.strong_count() > 0,
            }));
            MADE.with(|m| m.set(0));
            return;
        }
        collect();
    }
}

/// (collections run, containers freed by them) on this thread.
pub fn stats() -> (usize, usize) {
    STATS.with(Cell::get)
}

/// One collection; returns how many containers it freed.
pub fn collect() -> usize {
    let tracked = TRACKED.with(|t| std::mem::take(&mut *t.borrow_mut()));
    let live: Vec<Live> = tracked
        .iter()
        .filter_map(|t| match t {
            Tracked::List(w) => w.upgrade().map(Live::List),
            Tracked::Dict(w) => w.upgrade().map(Live::Dict),
            Tracked::Obj(w) => w.upgrade().map(Live::Obj),
        })
        .collect();
    drop(tracked);
    let index: HashMap<usize, usize> = live.iter().enumerate().map(|(i, l)| (l.addr(), i)).collect();

    // What the rest of the program holds of each container.
    let mut outside: Vec<isize> = live.iter().map(Live::count).collect();
    let mut children: Vec<Vec<usize>> = Vec::with_capacity(live.len());
    let mut borrowed = false;
    for l in &live {
        let mut mine = Vec::new();
        borrowed |= !l.children(&mut |a| {
            if let Some(&i) = index.get(&a) {
                mine.push(i);
            }
        });
        children.push(mine);
    }
    let freed = if borrowed {
        0
    } else {
        for c in children.iter().flatten() {
            outside[*c] -= 1;
        }
        // What the program can reach.
        let mut alive = vec![false; live.len()];
        let mut stack: Vec<usize> = (0..live.len()).filter(|&i| outside[i] > 0).collect();
        for &i in &stack {
            alive[i] = true;
        }
        while let Some(i) = stack.pop() {
            for &c in &children[i] {
                if !alive[c] {
                    alive[c] = true;
                    stack.push(c);
                }
            }
        }
        // The rest is cycles: empty them all, then let the contents go.
        let mut contents = Vec::new();
        let mut keys = Vec::new();
        let garbage: Vec<usize> = (0..live.len()).filter(|&i| !alive[i]).collect();
        for &i in &garbage {
            live[i].empty(&mut contents, &mut keys);
        }
        drop(contents);
        drop(keys);
        garbage.len()
    };

    // Survivors stay tracked (containers made during the drops are tracked already).
    let survivors: Vec<Tracked> = live
        .iter()
        .filter_map(|l| {
            let keep = match l {
                Live::List(r) => Rc::strong_count(r) > 1,
                Live::Dict(r) => Rc::strong_count(r) > 1,
                Live::Obj(r) => Rc::strong_count(r) > 1,
            };
            keep.then(|| match l {
                Live::List(r) => Tracked::List(Rc::downgrade(r)),
                Live::Dict(r) => Tracked::Dict(Rc::downgrade(r)),
                Live::Obj(r) => Tracked::Obj(Rc::downgrade(r)),
            })
        })
        .collect();
    let kept = survivors.len();
    drop(live);
    TRACKED.with(|t| {
        let mut t = t.borrow_mut();
        let made_during = std::mem::replace(&mut *t, survivors);
        t.extend(made_during);
    });
    MADE.with(|m| m.set(0));
    NEXT.with(|n| n.set(FLOOR.max(kept)));
    STATS.with(|s| {
        let (runs, total) = s.get();
        s.set((runs + 1, total + freed));
    });
    freed
}
