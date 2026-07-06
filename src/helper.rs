pub fn chunk_by<T, F>(vec: Vec<T>, mut predicate: F) -> Vec<Vec<T>>
where
    F: FnMut(&T) -> bool,
{
    let (mut result, last_chunk) =
        vec.into_iter()
            .fold((vec![], vec![]), |(mut result, mut chunk), i| {
                if !predicate(&i) && !chunk.is_empty() {
                    result.push(chunk);
                    chunk = vec![];
                }
                chunk.push(i);

                (result, chunk)
            });

    if !last_chunk.is_empty() {
        result.push(last_chunk);
    }

    result
}
