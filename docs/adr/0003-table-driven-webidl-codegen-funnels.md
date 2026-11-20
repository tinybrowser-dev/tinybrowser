# Table-driven WebIDL codegen with shared funnels

`webidl-bindgen` emits static member tables and shared `#[inline(never)]` conversion and interface-installation funnels monomorphized per IDL type rather than per member, and resolves `[SecureContext]` exposure once in the IDL database for both native and JS emitters. Dictionary conversions still deserialize unused members so WebIDL type errors fire on invalid present properties without hand-written glue.
