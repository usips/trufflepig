use tree_sitter::Node;

pub(super) fn declaration(node: Node<'_>) -> Option<(&str, Node<'_>)> {
    let mut kind = match node.kind() {
        "function_item"
        | "function_declaration"
        | "generator_function_declaration"
        | "function_signature"
        | "function_signature_item" => "function",
        "method_definition" | "method_signature" | "abstract_method_signature" => "method",
        "struct_item" => "struct",
        "enum_item" => "enum",
        "trait_item" => "trait",
        "mod_item" => "module",
        "class_declaration" | "abstract_class_declaration" => "class",
        "interface_declaration" => "interface",
        "type_item" | "type_alias_declaration" | "type_definition" => "type",
        "const_item" | "static_item" => "constant",
        "field_declaration" | "public_field_definition" | "property_signature" => "field",
        "enum_variant" => "variant",
        "macro_definition" => "macro",
        "import_specifier" | "namespace_import" => "import",
        "variable_declarator" => {
            let value = node.child_by_field_name("value");
            if value.is_some_and(|v| matches!(v.kind(), "arrow_function" | "function_expression")) {
                "function"
            } else {
                "variable"
            }
        }
        "let_declaration" => "variable",
        "parameter" | "required_parameter" | "optional_parameter" => "parameter",
        _ => return None,
    };
    if kind == "function"
        && node.parent().is_some_and(|parent| {
            parent.kind() == "declaration_list"
                && parent
                    .parent()
                    .is_some_and(|owner| matches!(owner.kind(), "impl_item" | "trait_item"))
        })
    {
        kind = "method";
    }
    let name = node
        .child_by_field_name("alias")
        .or_else(|| node.child_by_field_name("name"))
        .or_else(|| node.child_by_field_name("pattern"))
        .or_else(|| {
            (node.kind() == "parameter")
                .then(|| node.named_child(0))
                .flatten()
        })?;
    Some((kind, name))
}

pub(super) fn name_tokens<'a>(node: Node<'a>, result: &mut Vec<Node<'a>>) {
    if matches!(
        node.kind(),
        "identifier"
            | "type_identifier"
            | "field_identifier"
            | "property_identifier"
            | "shorthand_property_identifier_pattern"
    ) {
        result.push(node);
    } else if matches!(
        node.kind(),
        "tuple_pattern"
            | "tuple_struct_pattern"
            | "slice_pattern"
            | "array_pattern"
            | "object_pattern"
            | "mut_pattern"
            | "ref_pattern"
            | "reference_pattern"
            | "rest_pattern"
            | "required_parameter"
            | "optional_parameter"
    ) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            name_tokens(child, result);
        }
    } else if matches!(
        node.kind(),
        "pair_pattern" | "object_assignment_pattern" | "assignment_pattern"
    ) {
        if let Some(binding) = node
            .child_by_field_name("value")
            .or_else(|| node.child_by_field_name("left"))
        {
            name_tokens(binding, result);
        }
    } else if matches!(
        node.kind(),
        "dot_index_expression" | "method_index_expression"
    ) && let Some(field) = node
        .child_by_field_name("field")
        .or_else(|| node.child_by_field_name("method"))
    {
        result.push(field);
    }
}
