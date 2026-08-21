//! Implementation of a stack backed by a contiguous array / vector.
//!
//! Ported from `app.freerouting.datastructures.ArrayStack`.

/// A high-performance LIFO stack with pre-allocated capacity.
#[derive(Debug, Clone)]
pub struct ArrayStack<T> {
    data: Vec<T>,
}

impl<T> ArrayStack<T> {
    /// Creates a new `ArrayStack` with an initial capacity.
    pub fn new(capacity: usize) -> Self {
        ArrayStack {
            data: Vec::with_capacity(capacity),
        }
    }

    /// Resets the stack to empty, retaining allocated capacity.
    #[inline]
    pub fn reset(&mut self) {
        self.data.clear();
    }

    /// Pushes an element onto the top of the stack.
    #[inline]
    pub fn push(&mut self, element: T) {
        self.data.push(element);
    }

    /// Pops the next element from the top of the stack, returning `None` if empty.
    #[inline]
    pub fn pop(&mut self) -> Option<T> {
        self.data.pop()
    }

    /// Peeks at the top element without removing it.
    #[inline]
    pub fn peek(&self) -> Option<&T> {
        self.data.last()
    }

    /// Peeks mutably at the top element without removing it.
    #[inline]
    pub fn peek_mut(&mut self) -> Option<&mut T> {
        self.data.last_mut()
    }

    /// Returns `true` if the stack contains no elements.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Returns the number of elements in the stack.
    #[inline]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Returns the current allocated capacity.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.data.capacity()
    }

    /// Returns a slice of the stack elements from bottom to top.
    #[inline]
    pub fn as_slice(&self) -> &[T] {
        &self.data
    }
}

impl<T> Default for ArrayStack<T> {
    fn default() -> Self {
        Self::new(16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_pop() {
        let mut stack = ArrayStack::new(4);
        assert!(stack.is_empty());
        assert_eq!(stack.len(), 0);

        stack.push(10);
        stack.push(20);
        stack.push(30);

        assert_eq!(stack.len(), 3);
        assert_eq!(stack.peek(), Some(&30));
        assert_eq!(stack.pop(), Some(30));
        assert_eq!(stack.pop(), Some(20));
        assert_eq!(stack.pop(), Some(10));
        assert_eq!(stack.pop(), None);
        assert!(stack.is_empty());
    }

    #[test]
    fn test_reset() {
        let mut stack = ArrayStack::new(8);
        stack.push("a");
        stack.push("b");
        assert_eq!(stack.len(), 2);
        stack.reset();
        assert!(stack.is_empty());
        assert_eq!(stack.pop(), None);
    }
}
