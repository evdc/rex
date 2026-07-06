// Transitive closure over a small chain graph (SPEC §8):
// `path` is the least fixpoint of  path = edge + edge . path.

entity Node { name: Text }
entity Edge { src: NodeID, dst: NodeID }

let a = new Node { name: "a" }
let b = new Node { name: "b" }
let c = new Node { name: "c" }
let d = new Node { name: "d" }

let _ = new Edge { src: a, dst: b }
let _ = new Edge { src: b, dst: c }
let _ = new Edge { src: c, dst: d }

let srcof : Edge -> Node = :src
let dstof : Edge -> Node = :dst
let edge  : Node -> Node = dstof by srcof

let recursive path : Node -> Node = edge + edge . path
