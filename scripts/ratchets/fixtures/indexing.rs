// Counted: 6 index expressions and 3 slice expressions.
#[derive(Debug)]
pub struct Grid {
    cells: [[u8; 4]; 4],
    rows: Vec<[u8; 2]>,
}

fn reads(grid: &Grid, at: usize, text: &str, bytes: &[u8]) -> u8 {
    let first = grid.cells[at][0]; // two index expressions
    let row = grid.rows[at - 1]; // one
    let nested = bytes[usize::from(bytes[0])]; // two: outer and inner
    let head = &text[..at]; // a slice
    let tail = &bytes[at..=at + 1]; // a slice
    let ranged = &bytes[f(0..1)..]; // a slice: the range is at the top level
    first + row[0] // one
}

fn not_indexing(values: &mut [u8]) -> [u8; 2] {
    let literal = vec![1, 2, 3];
    let array = [0u8; 2];
    let [a, b] = array;
    let pattern = matches!(values, [_, ..]);
    let types: Box<[u8]> = Box::new([a, b]);
    let text = "x[0] and y[1..2]";
    let ch = '[';
    // z[0] in a comment
    [a, b]
}

#[cfg(test)]
mod tests {
    fn uncounted(v: &[u8]) -> u8 {
        v[0] + v[1..][0]
    }
}
