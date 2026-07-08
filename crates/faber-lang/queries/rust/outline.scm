; Faber outline query for Rust.
; @item    — the whole node (used for containment / byte-range)
; @name    — text shown in the breadcrumb / overlay
; @context — dimmed keyword prefix shown before @name in the overlay

(mod_item
  "mod" @context
  name: (identifier) @name) @item

(struct_item
  "struct" @context
  name: (type_identifier) @name) @item

(enum_item
  "enum" @context
  name: (type_identifier) @name) @item

(trait_item
  "trait" @context
  name: (type_identifier) @name) @item

; impl Foo  /  impl Trait for Foo  — show the implementing type as the name
(impl_item
  "impl" @context
  type: (_) @name) @item

(function_item
  "fn" @context
  name: (identifier) @name) @item

(type_item
  "type" @context
  name: (type_identifier) @name) @item

(const_item
  "const" @context
  name: (identifier) @name) @item

(macro_definition
  name: (identifier) @name) @item
