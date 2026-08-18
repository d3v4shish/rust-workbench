#![allow(dead_code, unused_variables)]

use std::cell::{Cell, OnceCell, RefCell, UnsafeCell};
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap, HashSet, LinkedList, VecDeque};
use std::ffi::OsString;
use std::path::PathBuf;
use std::pin::Pin;
use std::ptr::NonNull;
use std::rc::{Rc, Weak as RcWeak};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak as ArcWeak};

fn main() {
    let boxed = Box::new(1_i32);
    let boxed_slice: Box<[i32]> = vec![1, 2].into_boxed_slice();
    let pinned: Pin<Box<i32>> = Box::pin(2);

    let rc_cell = Rc::new(RefCell::new(vec![1_i32]));
    let rc_cell_clone = Rc::clone(&rc_cell);
    let rc_weak: RcWeak<RefCell<Vec<i32>>> = Rc::downgrade(&rc_cell);
    rc_cell_clone.borrow_mut().push(2);

    let arc_mutex = Arc::new(Mutex::new(vec![1_i32]));
    let arc_mutex_clone = Arc::clone(&arc_mutex);
    let arc_weak: ArcWeak<Mutex<Vec<i32>>> = Arc::downgrade(&arc_mutex);
    let mut mutex_guard = arc_mutex_clone.lock().unwrap();
    mutex_guard.push(2);
    drop(mutex_guard);

    let rw_lock = RwLock::new(String::from("value"));
    let read_guard = rw_lock.read().unwrap();
    drop(read_guard);

    let cell = Cell::new(1_u32);
    let unsafe_cell = UnsafeCell::new(1_u32);
    let once_cell = OnceCell::new();
    let once_lock = OnceLock::new();
    let _: Result<(), i32> = once_cell.set(1_i32);
    let _: Result<(), i32> = once_lock.set(1_i32);

    let vector = vec![1_i32, 2];
    let string = String::from("text");
    let deque: VecDeque<i32> = VecDeque::from([1, 2]);
    let heap: BinaryHeap<i32> = BinaryHeap::from([1, 2]);
    let path = PathBuf::from("path");
    let os = OsString::from("os");
    let map: HashMap<i32, i32> = HashMap::from([(1, 2)]);
    let set: HashSet<i32> = HashSet::from([1]);
    let tree_map: BTreeMap<i32, i32> = BTreeMap::from([(1, 2)]);
    let tree_set: BTreeSet<i32> = BTreeSet::from([1]);
    let list: LinkedList<i32> = LinkedList::from([1, 2]);

    let optional: Option<Box<i32>> = Some(Box::new(4));
    let outcome: Result<Box<i32>, ()> = Ok(Box::new(5));
    let tuple = (String::from("left"), 1_u8);
    let array = [1_u16, 2, 3];

    let target = String::from("borrowed");
    let shared: &str = &target;
    let slice: &[i32] = &vector;
    let raw: *const u16 = &array[0];
    let mut raw_target = 9_i32;
    let non_null = NonNull::from(&mut raw_target);

    assert_eq!(shared.len() + slice.len(), 10);
    assert!(!raw.is_null());
    assert_eq!(*boxed + *pinned, 3);
}
