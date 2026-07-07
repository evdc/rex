// The SPEC §12 worked example, in ASCII surface syntax.

entity Customer { name: Text, region: {@north + @south + @east + @west} }
entity Product  { name: Text, price: Money }
entity Order    { customer: CustomerID, placed: Date }
entity Line     { order: OrderID, product: ProductID, qty: Int }

let alice  = new Customer { name: "Alice", region: @west }
let bob    = new Customer { name: "Bob",   region: @east }
let widget = new Product  { name: "Widget", price: 9.99 }
let gizmo  = new Product  { name: "Gizmo",  price: 24.50 }
let o1 = new Order { customer: alice, placed: 2026-01-15 }
let o2 = new Order { customer: alice, placed: 2026-02-03 }
let o3 = new Order { customer: bob,   placed: 2026-02-20 }
let _  = new Line { order: o1, product: widget, qty: 3 }
let _  = new Line { order: o1, product: gizmo,  qty: 1 }
let _  = new Line { order: o2, product: widget, qty: 2 }
let _  = new Line { order: o3, product: gizmo,  qty: 5 }

// "West/East customers who spent over 30, with their total spend (qty * price)."
let lineprice : Line -> Money     = :qty * :product.price
let custspend : Customer -> Money = sum(lineprice by :order.customer)
let inregion  : Customer          = id where :region in (@west + @east)
// NOTE: §12 wrote `(custspend where > 30)[inregion]`, but under the strict §3.2
// rule `R[S]` joins R's RIGHT column (here Money) against S's left, so that form
// is ill-typed. Restricting by a customer key-set is composition on the shared
// Customer key: `inregion . (custspend where > 30)`.
let result    : Customer -> Money = inregion . (custspend where > 30)
