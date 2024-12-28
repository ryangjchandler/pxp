use snappers::snap;

mod parser;

use parser::{process, snapper};

// DocBlocks
snap!(
    snapper,
    docblock_empty,
    process("fixtures/docblocks/empty.php")
);
snap!(
    snapper,
    docblock_empty_multiline,
    process("fixtures/docblocks/empty-multiline.php")
);
snap!(
    snapper,
    docblock_text,
    process("fixtures/docblocks/text.php")
);
snap!(
    snapper,
    docblock_text_multiline,
    process("fixtures/docblocks/text-multiline.php")
);

snap!(
    snapper,
    docblock_param_empty,
    process("fixtures/docblocks/param-empty.php")
);
snap!(
    snapper,
    docblock_param_type,
    process("fixtures/docblocks/param-type.php")
);
snap!(
    snapper,
    docblock_param_nullable_type,
    process("fixtures/docblocks/param-nullable-type.php")
);
snap!(
    snapper,
    docblock_param_union_type,
    process("fixtures/docblocks/param-union-type.php")
);
snap!(
    snapper,
    docblock_param_intersection_type,
    process("fixtures/docblocks/param-intersection-type.php")
);
snap!(
    snapper,
    docblock_param_dnf_type,
    process("fixtures/docblocks/param-dnf-type.php")
);

snap!(
    snapper,
    docblock_param_variable,
    process("fixtures/docblocks/param-variable.php")
);
snap!(
    snapper,
    docblock_param_variable_description,
    process("fixtures/docblocks/param-variable-description.php")
);
snap!(
    snapper,
    docblock_param_type_variable,
    process("fixtures/docblocks/param-type-variable.php")
);
snap!(
    snapper,
    docblock_param_type_variable_description,
    process("fixtures/docblocks/param-type-variable-description.php")
);
snap!(
    snapper,
    docblock_param_simple_typed_array,
    process("fixtures/docblocks/param-simple-typed-array.php")
);
snap!(
    snapper,
    docblock_param_nested_typed_array,
    process("fixtures/docblocks/param-nested-typed-array.php")
);
snap!(
    snapper,
    docblock_param_union_typed_array,
    process("fixtures/docblocks/param-union-typed-array.php")
);
snap!(
    snapper,
    docblock_param_unparenthesized_union_typed_array,
    process("fixtures/docblocks/param-unparenthesized-union-typed-array.php")
);
snap!(
    snapper,
    docblock_param_intersection_typed_array,
    process("fixtures/docblocks/param-intersection-typed-array.php")
);
snap!(
    snapper,
    docblock_param_simple_generic_type,
    process("fixtures/docblocks/param-simple-generic-type.php")
);
snap!(
    snapper,
    docblock_param_multiple_generic_type,
    process("fixtures/docblocks/param-multiple-generic-type.php")
);
snap!(
    snapper,
    docblock_param_nested_generic_type,
    process("fixtures/docblocks/param-nested-generic-type.php")
);

snap!(
    snapper,
    docblock_var_empty,
    process("fixtures/docblocks/var-empty.php")
);
snap!(
    snapper,
    docblock_var_type,
    process("fixtures/docblocks/var-type.php")
);
snap!(
    snapper,
    docblock_var_type_text,
    process("fixtures/docblocks/var-type-text.php")
);
snap!(
    snapper,
    docblock_var_type_variable,
    process("fixtures/docblocks/var-type-variable.php")
);
snap!(
    snapper,
    docblock_var_type_variable_text,
    process("fixtures/docblocks/var-type-variable-text.php")
);
snap!(
    snapper,
    docblock_array_shape,
    process("fixtures/docblocks/array-shape.php")
);
snap!(
    snapper,
    docblock_array_shape_keyless,
    process("fixtures/docblocks/array-shape-keyless.php")
);
snap!(
    snapper,
    docblock_array_shape_int_keyed,
    process("fixtures/docblocks/array-shape-int-keyed.php")
);
snap!(
    snapper,
    docblock_array_shape_ident_keyed,
    process("fixtures/docblocks/array-shape-ident-keyed.php")
);
snap!(
    snapper,
    docblock_array_shape_string_keyed,
    process("fixtures/docblocks/array-shape-string-keyed.php")
);
snap!(
    snapper,
    docblock_array_shape_mixed_keyed,
    process("fixtures/docblocks/array-shape-mixed-keyed.php")
);
snap!(
    snapper,
    docblock_array_shape_unsealed,
    process("fixtures/docblocks/array-shape-unsealed.php")
);
snap!(
    snapper,
    docblock_array_shape_unsealed_list,
    process("fixtures/docblocks/array-shape-unsealed-list.php")
);
snap!(
    snapper,
    docblock_array_shape_variadic,
    process("fixtures/docblocks/array-shape-variadic.php")
);
snap!(snapper, docblock_property_tag, process("fixtures/docblocks/property-tag.php"));
snap!(snapper, docblock_property_read_tag, process("fixtures/docblocks/property-read-tag.php"));
snap!(snapper, docblock_property_write_tag, process("fixtures/docblocks/property-write-tag.php"));
