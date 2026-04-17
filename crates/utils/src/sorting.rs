//! Sorting algorithms module.
//!
//! Provides generic sorting utilities that work with any comparable type.

/// In-place quicksort implementation.
///
/// Sorts the slice in ascending order using the quicksort algorithm.
/// This is a generic implementation that works with any type implementing `PartialOrd`.
///
/// # Arguments
///
/// * `arr` - The slice to sort in-place
///
/// # Examples
///
/// ```
/// use clawcr_utils::sorting::quicksort;
///
/// let mut nums = [5, 2, 8, 1, 9];
/// quicksort(&mut nums);
/// assert_eq!(nums, [1, 2, 5, 8, 9]);
/// ```
pub fn quicksort<T: PartialOrd>(arr: &mut [T]) {
    if arr.len() <= 1 {
        return;
    }
    quicksort_recursive(arr, 0, arr.len() - 1);
}

fn quicksort_recursive<T: PartialOrd>(arr: &mut [T], low: usize, high: usize) {
    if low >= high {
        return;
    }

    let pivot_index = partition(arr, low, high);

    if pivot_index > 0 {
        quicksort_recursive(arr, low, pivot_index - 1);
    }
    quicksort_recursive(arr, pivot_index + 1, high);
}

fn partition<T: PartialOrd>(arr: &mut [T], low: usize, high: usize) -> usize {
    let high_idx = high;
    let mut i = low;
    let mut j = high;

    while i < j {
        while i < high_idx && arr[i] <= arr[high_idx] {
            i += 1;
        }
        while j > low && arr[j] > arr[high_idx] {
            j -= 1;
        }
        if i < j {
            arr.swap(i, j);
        }
    }
    arr.swap(i, high_idx);
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_empty_array() {
        let mut empty: Vec<i32> = vec![];
        quicksort(&mut empty);
        assert_eq!(empty, []);
    }

    #[test]
    fn test_single_element() {
        let mut single = [42];
        quicksort(&mut single);
        assert_eq!(single, [42]);
    }

    #[test]
    fn test_two_elements_sorted() {
        let mut sorted = [1, 2];
        quicksort(&mut sorted);
        assert_eq!(sorted, [1, 2]);
    }

    #[test]
    fn test_two_elements_unsorted() {
        let mut unsorted = [2, 1];
        quicksort(&mut unsorted);
        assert_eq!(unsorted, [1, 2]);
    }

    #[test]
    fn test_already_sorted() {
        let mut sorted = [1, 2, 3, 4, 5];
        quicksort(&mut sorted);
        assert_eq!(sorted, [1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_reverse_sorted() {
        let mut reverse = [5, 4, 3, 2, 1];
        quicksort(&mut reverse);
        assert_eq!(reverse, [1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_random_order() {
        let mut random = [3, 1, 4, 1, 5, 9, 2, 6];
        quicksort(&mut random);
        assert_eq!(random, [1, 1, 2, 3, 4, 5, 6, 9]);
    }

    #[test]
    fn test_with_duplicates() {
        let mut duplicates = [5, 2, 8, 2, 9, 1, 5, 8];
        quicksort(&mut duplicates);
        assert_eq!(duplicates, [1, 2, 2, 5, 5, 8, 8, 9]);
    }

    #[test]
    fn test_negative_numbers() {
        let mut negatives = [-3, -1, -7, 0, 2, -5];
        quicksort(&mut negatives);
        assert_eq!(negatives, [-7, -5, -3, -1, 0, 2]);
    }

    #[test]
    fn test_strings() {
        let mut strings = ["banana", "apple", "cherry", "date"];
        quicksort(&mut strings);
        assert_eq!(strings, ["apple", "banana", "cherry", "date"]);
    }

    #[test]
    fn test_large_array() {
        let mut large: Vec<i32> = (0..1000).rev().collect();
        quicksort(&mut large);
        assert_eq!(large, (0..1000).collect::<Vec<_>>());
    }

    #[test]
    fn test_char_array() {
        let mut chars = ['z', 'a', 'm', 'b'];
        quicksort(&mut chars);
        assert_eq!(chars, ['a', 'b', 'm', 'z']);
    }

    #[test]
    fn test_with_all_equal_elements() {
        let mut equal = [7, 7, 7, 7, 7];
        quicksort(&mut equal);
        assert_eq!(equal, [7, 7, 7, 7, 7]);
    }

    #[test]
    fn test_unsigned_integers() {
        let mut unsigned = [10u32, 5, 8, 3, 2, 9, 1];
        quicksort(&mut unsigned);
        assert_eq!(unsigned, [1, 2, 3, 5, 8, 9, 10]);
    }

    #[test]
    fn test_floating_point() {
        let mut floats = [3.14, 1.41, 2.71, 0.0, -1.0];
        quicksort(&mut floats);
        assert_eq!(floats, [-1.0, 0.0, 1.41, 2.71, 3.14]);
    }
}
