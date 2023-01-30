pub enum MaybeOwned<'a, T> {
    Owned(T),
    Borrowed(&'a T)
}