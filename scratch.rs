#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VecRelation {
    index: Vec<(Value, i64)>
}

impl BinaryRelation for VecRelation {
    fn add(&mut self, left: Value, right: Value, weight: i64) {
        let Value::Id(sort, idx) = left else {
            panic!("a VecRelation can only hold Entity IDs in its left domain")
        };
        let idx = idx as usize;
        // assert sort id matches - this is supposed to have entities of one kind
        if idx < self.index.len() {
            let (existing, w ) = self.index[idx];
            // if existing == right then add weight
            // else replace?
        } else {
            self.index.push((right, weight));
        }
    }
}