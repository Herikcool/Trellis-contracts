#![cfg(test)]

extern crate std;

use soroban_sdk::{Env, Vec};
use crate::pagination::{
    paginate_id_list, paginate_id_range, Direction, PageRequest, DEFAULT_MAX_SCAN,
};

#[test]
fn test_sequential_ascending_pagination() {
    let env = Env::default();
    let total_count = 10u64;

    // Page 1: limit 4, cursor None
    let req1 = PageRequest::ascending(None, 4);
    let page1 = paginate_id_range(&env, total_count, &req1, DEFAULT_MAX_SCAN, |id| Some(id)).unwrap();
    assert_eq!(page1.items.len(), 4);
    assert_eq!(page1.items.get(0).unwrap(), 0);
    assert_eq!(page1.items.get(3).unwrap(), 3);
    assert_eq!(page1.next_cursor, Some(3));
    assert!(page1.has_more);

    // Page 2: limit 4, cursor Some(3)
    let req2 = PageRequest::ascending(page1.next_cursor, 4);
    let page2 = paginate_id_range(&env, total_count, &req2, DEFAULT_MAX_SCAN, |id| Some(id)).unwrap();
    assert_eq!(page2.items.len(), 4);
    assert_eq!(page2.items.get(0).unwrap(), 4);
    assert_eq!(page2.items.get(3).unwrap(), 7);
    assert_eq!(page2.next_cursor, Some(7));
    assert!(page2.has_more);

    // Page 3: limit 4, cursor Some(7)
    let req3 = PageRequest::ascending(page2.next_cursor, 4);
    let page3 = paginate_id_range(&env, total_count, &req3, DEFAULT_MAX_SCAN, |id| Some(id)).unwrap();
    assert_eq!(page3.items.len(), 2);
    assert_eq!(page3.items.get(0).unwrap(), 8);
    assert_eq!(page3.items.get(1).unwrap(), 9);
    assert_eq!(page3.next_cursor, None);
    assert!(!page3.has_more);
}

#[test]
fn test_stability_under_concurrent_inserts() {
    let env = Env::default();
    let mut total_count = 10u64;

    // Read Page 1 (IDs 0..4)
    let req1 = PageRequest::ascending(None, 5);
    let page1 = paginate_id_range(&env, total_count, &req1, DEFAULT_MAX_SCAN, |id| Some(id)).unwrap();
    assert_eq!(page1.items.len(), 5);
    assert_eq!(page1.next_cursor, Some(4));

    // Concurrently insert 5 new records (total_count becomes 15)
    total_count += 5;

    // Read Page 2 with cursor Some(4)
    let req2 = PageRequest::ascending(page1.next_cursor, 5);
    let page2 = paginate_id_range(&env, total_count, &req2, DEFAULT_MAX_SCAN, |id| Some(id)).unwrap();
    assert_eq!(page2.items.len(), 5);
    assert_eq!(page2.items.get(0).unwrap(), 5);
    assert_eq!(page2.items.get(4).unwrap(), 9);
    assert_eq!(page2.next_cursor, Some(9));

    // Read Page 3 (newly inserted records)
    let req3 = PageRequest::ascending(page2.next_cursor, 5);
    let page3 = paginate_id_range(&env, total_count, &req3, DEFAULT_MAX_SCAN, |id| Some(id)).unwrap();
    assert_eq!(page3.items.len(), 5);
    assert_eq!(page3.items.get(0).unwrap(), 10);
    assert_eq!(page3.items.get(4).unwrap(), 14);
    assert_eq!(page3.next_cursor, None);
}

#[test]
fn test_stability_under_concurrent_deletes_and_filtering() {
    let env = Env::default();
    let total_count = 10u64;

    // Simulate active status where items 1 and 3 are deleted/hidden
    let mut active = [true; 10];

    // Page 1
    let req1 = PageRequest::ascending(None, 3);
    let page1 = paginate_id_range(&env, total_count, &req1, DEFAULT_MAX_SCAN, |id| {
        if active[id as usize] {
            Some(id)
        } else {
            None
        }
    }).unwrap();
    assert_eq!(page1.items.len(), 3);
    assert_eq!(page1.items.get(0).unwrap(), 0);
    assert_eq!(page1.items.get(1).unwrap(), 1);
    assert_eq!(page1.items.get(2).unwrap(), 2);
    assert_eq!(page1.next_cursor, Some(2));

    // Now delete items 0 and 1
    active[0] = false;
    active[1] = false;

    // Page 2 resumes from cursor Some(2), seeking strictly > 2
    let req2 = PageRequest::ascending(page1.next_cursor, 3);
    let page2 = paginate_id_range(&env, total_count, &req2, DEFAULT_MAX_SCAN, |id| {
        if active[id as usize] {
            Some(id)
        } else {
            None
        }
    }).unwrap();

    assert_eq!(page2.items.len(), 3);
    assert_eq!(page2.items.get(0).unwrap(), 3);
    assert_eq!(page2.items.get(1).unwrap(), 4);
    assert_eq!(page2.items.get(2).unwrap(), 5);
    assert_eq!(page2.next_cursor, Some(5));
}

#[test]
fn test_descending_pagination() {
    let env = Env::default();
    let total_count = 10u64;

    // Page 1 descending: limit 4, cursor None (starts at 9)
    let req1 = PageRequest::descending(None, 4);
    let page1 = paginate_id_range(&env, total_count, &req1, DEFAULT_MAX_SCAN, |id| Some(id)).unwrap();
    assert_eq!(page1.items.len(), 4);
    assert_eq!(page1.items.get(0).unwrap(), 9);
    assert_eq!(page1.items.get(3).unwrap(), 6);
    assert_eq!(page1.next_cursor, Some(6));

    // Page 2 descending: cursor Some(6) -> items 5, 4, 3, 2
    let req2 = PageRequest::descending(page1.next_cursor, 4);
    let page2 = paginate_id_range(&env, total_count, &req2, DEFAULT_MAX_SCAN, |id| Some(id)).unwrap();
    assert_eq!(page2.items.len(), 4);
    assert_eq!(page2.items.get(0).unwrap(), 5);
    assert_eq!(page2.items.get(3).unwrap(), 2);
    assert_eq!(page2.next_cursor, Some(2));

    // Page 3 descending: cursor Some(2) -> items 1, 0
    let req3 = PageRequest::descending(page2.next_cursor, 4);
    let page3 = paginate_id_range(&env, total_count, &req3, DEFAULT_MAX_SCAN, |id| Some(id)).unwrap();
    assert_eq!(page3.items.len(), 2);
    assert_eq!(page3.items.get(0).unwrap(), 1);
    assert_eq!(page3.items.get(1).unwrap(), 0);
    assert_eq!(page3.next_cursor, None);
}

#[test]
fn test_paginate_id_list() {
    let env = Env::default();
    let mut ids = Vec::new(&env);
    ids.push_back(10);
    ids.push_back(25);
    ids.push_back(30);
    ids.push_back(45);
    ids.push_back(50);

    let req1 = PageRequest::ascending(None, 2);
    let page1 = paginate_id_list(&env, &ids, &req1, DEFAULT_MAX_SCAN, |id| Some(id * 2)).unwrap();
    assert_eq!(page1.items.len(), 2);
    assert_eq!(page1.items.get(0).unwrap(), 20); // 10 * 2
    assert_eq!(page1.items.get(1).unwrap(), 50); // 25 * 2
    assert_eq!(page1.next_cursor, Some(25));

    let req2 = PageRequest::ascending(page1.next_cursor, 2);
    let page2 = paginate_id_list(&env, &ids, &req2, DEFAULT_MAX_SCAN, |id| Some(id * 2)).unwrap();
    assert_eq!(page2.items.len(), 2);
    assert_eq!(page2.items.get(0).unwrap(), 60); // 30 * 2
    assert_eq!(page2.items.get(1).unwrap(), 90); // 45 * 2
    assert_eq!(page2.next_cursor, Some(45));

    let req3 = PageRequest::ascending(page2.next_cursor, 2);
    let page3 = paginate_id_list(&env, &ids, &req3, DEFAULT_MAX_SCAN, |id| Some(id * 2)).unwrap();
    assert_eq!(page3.items.len(), 1);
    assert_eq!(page3.items.get(0).unwrap(), 100); // 50 * 2
    assert_eq!(page3.next_cursor, None);
    assert!(!page3.has_more);
}

#[test]
fn test_descending_paginate_id_list() {
    let env = Env::default();
    let mut ids = Vec::new(&env);
    ids.push_back(10);
    ids.push_back(20);
    ids.push_back(30);
    ids.push_back(40);
    ids.push_back(50);

    // Page 1 descending: limit 2, cursor None -> should return 50, 40
    let req1 = PageRequest::descending(None, 2);
    let page1 = paginate_id_list(&env, &ids, &req1, DEFAULT_MAX_SCAN, |id| Some(id)).unwrap();
    assert_eq!(page1.items.len(), 2);
    assert_eq!(page1.items.get(0).unwrap(), 50);
    assert_eq!(page1.items.get(1).unwrap(), 40);
    assert_eq!(page1.next_cursor, Some(40));
    assert!(page1.has_more);

    // Page 2 descending: limit 2, cursor Some(40) -> should return 30, 20
    let req2 = PageRequest::descending(page1.next_cursor, 2);
    let page2 = paginate_id_list(&env, &ids, &req2, DEFAULT_MAX_SCAN, |id| Some(id)).unwrap();
    assert_eq!(page2.items.len(), 2);
    assert_eq!(page2.items.get(0).unwrap(), 30);
    assert_eq!(page2.items.get(1).unwrap(), 20);
    assert_eq!(page2.next_cursor, Some(20));
    assert!(page2.has_more);

    // Page 3 descending: limit 2, cursor Some(20) -> should return 10
    let req3 = PageRequest::descending(page2.next_cursor, 2);
    let page3 = paginate_id_list(&env, &ids, &req3, DEFAULT_MAX_SCAN, |id| Some(id)).unwrap();
    assert_eq!(page3.items.len(), 1);
    assert_eq!(page3.items.get(0).unwrap(), 10);
    assert_eq!(page3.next_cursor, None);
    assert!(!page3.has_more);
}

#[test]
fn test_paginate_id_list_stability_under_concurrent_inserts() {
    let env = Env::default();
    let mut ids = Vec::new(&env);
    ids.push_back(10);
    ids.push_back(20);
    ids.push_back(30);
    ids.push_back(40);

    // Page 1: reads 10, 20
    let req1 = PageRequest::ascending(None, 2);
    let page1 = paginate_id_list(&env, &ids, &req1, DEFAULT_MAX_SCAN, |id| Some(id)).unwrap();
    assert_eq!(page1.items.len(), 2);
    assert_eq!(page1.next_cursor, Some(20));

    // Concurrently insert 5 (before cursor), 25 (between cursor and future), 50 (after future)
    let mut new_ids = Vec::new(&env);
    new_ids.push_back(5);
    new_ids.push_back(10);
    new_ids.push_back(20);
    new_ids.push_back(25);
    new_ids.push_back(30);
    new_ids.push_back(40);
    new_ids.push_back(50);

    // Page 2: with cursor Some(20) resumes strictly > 20
    let req2 = PageRequest::ascending(page1.next_cursor, 2);
    let page2 = paginate_id_list(&env, &new_ids, &req2, DEFAULT_MAX_SCAN, |id| Some(id)).unwrap();
    assert_eq!(page2.items.len(), 2);
    assert_eq!(page2.items.get(0).unwrap(), 25);
    assert_eq!(page2.items.get(1).unwrap(), 30);
    assert_eq!(page2.next_cursor, Some(30));

    // Page 3: resumes strictly > 30 -> 40, 50
    let req3 = PageRequest::ascending(page2.next_cursor, 2);
    let page3 = paginate_id_list(&env, &new_ids, &req3, DEFAULT_MAX_SCAN, |id| Some(id)).unwrap();
    assert_eq!(page3.items.len(), 2);
    assert_eq!(page3.items.get(0).unwrap(), 40);
    assert_eq!(page3.items.get(1).unwrap(), 50);
    assert_eq!(page3.next_cursor, None);
}

#[test]
fn test_paginate_id_list_stability_under_concurrent_deletes() {
    let env = Env::default();
    let mut ids = Vec::new(&env);
    ids.push_back(10);
    ids.push_back(20);
    ids.push_back(30);
    ids.push_back(40);
    ids.push_back(50);

    // Page 1: reads 10, 20 -> cursor Some(20)
    let req1 = PageRequest::ascending(None, 2);
    let page1 = paginate_id_list(&env, &ids, &req1, DEFAULT_MAX_SCAN, |id| Some(id)).unwrap();
    assert_eq!(page1.items.len(), 2);
    assert_eq!(page1.next_cursor, Some(20));

    // Concurrently delete 10 (seen), 20 (cursor itself!), and 30 (unseen)
    let mut remaining_ids = Vec::new(&env);
    remaining_ids.push_back(40);
    remaining_ids.push_back(50);

    // Page 2: with cursor Some(20), binary search still seeks strictly > 20
    let req2 = PageRequest::ascending(page1.next_cursor, 2);
    let page2 = paginate_id_list(&env, &remaining_ids, &req2, DEFAULT_MAX_SCAN, |id| Some(id)).unwrap();
    assert_eq!(page2.items.len(), 2);
    assert_eq!(page2.items.get(0).unwrap(), 40);
    assert_eq!(page2.items.get(1).unwrap(), 50);
    assert_eq!(page2.next_cursor, None);
}

#[test]
fn test_paginate_id_list_permission_revocation_never_leaks_records() {
    let env = Env::default();
    let mut ids = Vec::new(&env);
    ids.push_back(10);
    ids.push_back(20);
    ids.push_back(30);
    ids.push_back(40);
    ids.push_back(50);

    // Simulated permission table for viewer
    let mut allowed = [true; 60];

    // Page 1: viewer has access to all records
    let req1 = PageRequest::ascending(None, 2);
    let page1 = paginate_id_list(&env, &ids, &req1, DEFAULT_MAX_SCAN, |id| {
        if allowed[id as usize] {
            Some(id)
        } else {
            None
        }
    }).unwrap();
    assert_eq!(page1.items.len(), 2);
    assert_eq!(page1.items.get(0).unwrap(), 10);
    assert_eq!(page1.items.get(1).unwrap(), 20);
    assert_eq!(page1.next_cursor, Some(20));

    // Mid-traversal permission revocation: record 30 is newly forbidden / hidden
    allowed[30] = false;

    // Page 2: seeking > 20 evaluates 30, skips it due to revoked permission, returns 40 and 50
    let req2 = PageRequest::ascending(page1.next_cursor, 2);
    let page2 = paginate_id_list(&env, &ids, &req2, DEFAULT_MAX_SCAN, |id| {
        if allowed[id as usize] {
            Some(id)
        } else {
            None
        }
    }).unwrap();
    assert_eq!(page2.items.len(), 2);
    assert_eq!(page2.items.get(0).unwrap(), 40);
    assert_eq!(page2.items.get(1).unwrap(), 50);
    assert_eq!(page2.next_cursor, None);
}

#[test]
fn test_max_scan_sparse_cluster_yield_and_resume() {
    let env = Env::default();
    let mut ids = Vec::new(&env);
    for i in 1..=10 {
        ids.push_back(i);
    }

    // Only IDs 9 and 10 are visible; 1..8 are forbidden/deleted
    let req1 = PageRequest::ascending(None, 5);
    // Limit max_scan to 4: only IDs 1..4 will be scanned in call 1
    let page1 = paginate_id_list(&env, &ids, &req1, 4, |id| {
        if id >= 9 {
            Some(id)
        } else {
            None
        }
    }).unwrap();

    assert_eq!(page1.items.len(), 0); // No items found within max_scan limit
    assert_eq!(page1.scanned_count, 4);
    assert_eq!(page1.next_cursor, Some(4)); // Yields resume cursor
    assert!(page1.has_more);

    // Call 2: scan next 4 items (IDs 5..8)
    let req2 = PageRequest::ascending(page1.next_cursor, 5);
    let page2 = paginate_id_list(&env, &ids, &req2, 4, |id| {
        if id >= 9 {
            Some(id)
        } else {
            None
        }
    }).unwrap();

    assert_eq!(page2.items.len(), 0);
    assert_eq!(page2.scanned_count, 4);
    assert_eq!(page2.next_cursor, Some(8));
    assert!(page2.has_more);

    // Call 3: scan remaining items (IDs 9 and 10)
    let req3 = PageRequest::ascending(page2.next_cursor, 5);
    let page3 = paginate_id_list(&env, &ids, &req3, 4, |id| {
        if id >= 9 {
            Some(id)
        } else {
            None
        }
    }).unwrap();

    assert_eq!(page3.items.len(), 2);
    assert_eq!(page3.items.get(0).unwrap(), 9);
    assert_eq!(page3.items.get(1).unwrap(), 10);
    assert_eq!(page3.next_cursor, None);
    assert!(!page3.has_more);
}

#[test]
fn test_boundary_conditions() {
    let env = Env::default();
    let empty_ids = Vec::new(&env);

    let req = PageRequest::ascending(None, 5);
    let page = paginate_id_list(&env, &empty_ids, &req, DEFAULT_MAX_SCAN, |id| Some(id)).unwrap();
    assert_eq!(page.items.len(), 0);
    assert_eq!(page.next_cursor, None);
    assert!(!page.has_more);

    let mut ids = Vec::new(&env);
    ids.push_back(100);

    // Cursor past max
    let req_past = PageRequest::ascending(Some(100), 5);
    let page_past = paginate_id_list(&env, &ids, &req_past, DEFAULT_MAX_SCAN, |id| Some(id)).unwrap();
    assert_eq!(page_past.items.len(), 0);
    assert_eq!(page_past.next_cursor, None);

    // Descending cursor before min
    let req_desc_past = PageRequest::descending(Some(100), 5);
    let page_desc_past = paginate_id_list(&env, &ids, &req_desc_past, DEFAULT_MAX_SCAN, |id| Some(id)).unwrap();
    assert_eq!(page_desc_past.items.len(), 0);
    assert_eq!(page_desc_past.next_cursor, None);
}
