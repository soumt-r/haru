//! Go's `sort.SliceStable`, step for step: with an order that is not
//! consistent (NaN keys) it still ends as Go's does, and it never panics.

pub fn stable<T>(data: &mut [T], less: impl Fn(&T, &T) -> bool) {
    let n = data.len();
    let mut block = 20;
    let (mut a, mut b) = (0, block);
    while b <= n {
        insertion(data, &less, a, b);
        a = b;
        b += block;
    }
    insertion(data, &less, a, n);
    while block < n {
        a = 0;
        b = 2 * block;
        while b <= n {
            sym_merge(data, &less, a, a + block, b);
            a = b;
            b += 2 * block;
        }
        let m = a + block;
        if m < n {
            sym_merge(data, &less, a, m, n);
        }
        block *= 2;
    }
}

fn insertion<T>(data: &mut [T], less: &impl Fn(&T, &T) -> bool, a: usize, b: usize) {
    for i in a + 1..b {
        let mut j = i;
        while j > a && less(&data[j], &data[j - 1]) {
            data.swap(j, j - 1);
            j -= 1;
        }
    }
}

fn sym_merge<T>(data: &mut [T], less: &impl Fn(&T, &T) -> bool, a: usize, m: usize, b: usize) {
    if m - a == 1 {
        let (mut i, mut j) = (m, b);
        while i < j {
            let h = (i + j) / 2;
            if less(&data[h], &data[a]) {
                i = h + 1;
            } else {
                j = h;
            }
        }
        let mut k = a;
        while k + 1 < i {
            data.swap(k, k + 1);
            k += 1;
        }
        return;
    }
    if b - m == 1 {
        let (mut i, mut j) = (a, m);
        while i < j {
            let h = (i + j) / 2;
            if !less(&data[m], &data[h]) {
                i = h + 1;
            } else {
                j = h;
            }
        }
        let mut k = m;
        while k > i {
            data.swap(k, k - 1);
            k -= 1;
        }
        return;
    }
    let mid = (a + b) / 2;
    let n = mid + m;
    let (mut start, mut r) = if m > mid { (n - b, mid) } else { (a, m) };
    let p = n - 1;
    while start < r {
        let c = (start + r) / 2;
        if !less(&data[p - c], &data[c]) {
            start = c + 1;
        } else {
            r = c;
        }
    }
    let end = n - start;
    if start < m && m < end {
        rotate(data, start, m, end);
    }
    if a < start && start < mid {
        sym_merge(data, less, a, start, mid);
    }
    if mid < end && end < b {
        sym_merge(data, less, mid, end, b);
    }
}

fn swap_range<T>(data: &mut [T], a: usize, b: usize, n: usize) {
    for i in 0..n {
        data.swap(a + i, b + i);
    }
}

fn rotate<T>(data: &mut [T], a: usize, m: usize, b: usize) {
    let (mut i, mut j) = (m - a, b - m);
    while i != j {
        if i > j {
            swap_range(data, m - i, m, j);
            i -= j;
        } else {
            swap_range(data, m - i, m + j - i, i);
            j -= i;
        }
    }
    swap_range(data, m - i, m, i);
}
