use std::time::Duration;

use super::super::{Definition, Extraction, Occurrence};
use super::extract;

fn definition<'a>(extraction: &'a Extraction, name: &str, kind: &str) -> &'a Definition {
    extraction
        .definitions
        .iter()
        .find(|definition| definition.name == name && definition.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind} definition {name}"))
}

fn occurrence<'a>(
    extraction: &'a Extraction,
    source: &[u8],
    token: &[u8],
    role: &str,
    name: &str,
) -> &'a Occurrence {
    extraction
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == role
                && occurrence.name == name
                && source.get(occurrence.start..occurrence.end) == Some(token)
        })
        .unwrap_or_else(|| panic!("missing {role} occurrence {name} at {token:?}"))
}

fn source_span(source: &[u8], start: usize, end: usize) -> &[u8] {
    source
        .get(start..end)
        .expect("span must point into source bytes")
}

#[test]
fn declarations_keep_names_containers_and_source_byte_spans() {
    let source = r#"<?php
namespace Acme\Store;

// π shifts later byte positions beyond character positions.
class Base {} interface Reader { public function read(Record $record): Result; }
trait Logs { public function log(string $message): void { $local = $message; } } enum Status: string { case Ready = 'ready'; }
class Service extends Base implements Reader {
    use Logs; public const DEFAULT = 1;
    public string $label; public function read(Record $record, string $suffix = ''): Result {
        $copy = $record;
        return Formatter::format($copy);
    }
}
function helper($input) { $local = $input; return $local; }
"#;
    let source = source.as_bytes();

    let extracted = extract(source);
    assert_eq!(extracted.status, "complete");

    for (name, kind) in [
        ("Acme\\Store", "namespace"),
        ("Base", "class"),
        ("Reader", "interface"),
        ("Logs", "trait"),
        ("Status", "enum"),
        ("Service", "class"),
        ("helper", "function"),
        ("label", "property"),
        ("DEFAULT", "constant"),
        ("Ready", "enum_case"),
        ("$record", "parameter"),
        ("$copy", "variable"),
    ] {
        definition(&extracted, name, kind);
    }

    let service = definition(&extracted, "Service", "class");
    assert_eq!(service.container.as_deref(), Some("Acme\\Store"));
    let helper = definition(&extracted, "helper", "function");
    assert_eq!(helper.container.as_deref(), Some("Acme\\Store"));
    let service_read = extracted
        .definitions
        .iter()
        .find(|definition| {
            definition.name == "read"
                && definition.kind == "method"
                && definition.container.as_deref() == Some("Acme\\Store\\Service")
        })
        .expect("Service::read definition");
    assert!(
        source_span(source, service_read.start, service_read.end)
            .starts_with(b"public function read")
    );
    assert!(
        source_span(source, service.start, service.end).starts_with(b"class Service extends Base")
    );
    let label = definition(&extracted, "label", "property");
    assert_eq!(label.container.as_deref(), Some("Acme\\Store\\Service"));
    assert_eq!(source_span(source, label.start, label.end), b"$label");
    let constant = definition(&extracted, "DEFAULT", "constant");
    assert_eq!(constant.container.as_deref(), Some("Acme\\Store\\Service"));

    let copy_declaration = occurrence(&extracted, source, b"$copy", "declaration", "$copy");
    assert_eq!(
        source_span(source, copy_declaration.start, copy_declaration.end),
        b"$copy"
    );
    let copy_index = extracted
        .definitions
        .iter()
        .position(|definition| definition.name == "$copy" && definition.kind == "variable")
        .expect("local variable definition");
    let copy_read = occurrence(&extracted, source, b"$copy", "read", "$copy");
    assert_eq!(copy_read.target, None);
    assert!(copy_read.candidates.contains(&copy_index));
    let record_read = occurrence(&extracted, source, b"$record", "read", "$record");
    assert_eq!(
        extracted.definitions[record_read.target.unwrap()].kind,
        "parameter"
    );
    let parameter = definition(&extracted, "$record", "parameter");
    assert_eq!(
        source_span(source, parameter.start, parameter.end),
        b"Record $record"
    );

    assert!(
        extracted
            .occurrences
            .iter()
            .all(|occurrence| source.get(occurrence.start..occurrence.end).is_some())
    );
}

#[test]
fn imports_normalize_aliases_without_leaking_between_namespace_blocks() {
    let source = br#"<?php
namespace App {
    use Vendor\Widget\Card as Tile;
    use function Vendor\Format\render as draw;
    use const Vendor\Config\SHADE as Shade;
    class First extends Tile {
        public function render(Tile $value) { return [draw(), Shade, shade]; }
    }
}
namespace App {
    use Vendor\Other\Card as Tile;
    class Second extends Tile {}
}
"#;

    let extracted = extract(source);
    assert_eq!(extracted.status, "complete");
    for (target, provenance) in [
        ("Vendor\\Widget\\Card", "php_import"),
        ("Vendor\\Format\\render", "php_function_import"),
        ("Vendor\\Config\\SHADE", "php_const_import"),
        ("Vendor\\Other\\Card", "php_import"),
    ] {
        let imported = extracted
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == "import"
                    && occurrence.name == target
                    && occurrence.provenance == provenance
            })
            .unwrap_or_else(|| panic!("missing import {target} with {provenance}"));
        assert_eq!(
            source_span(source, imported.start, imported.end),
            target.as_bytes()
        );
    }

    let tile_targets: Vec<_> = extracted
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == "type"
                && source.get(occurrence.start..occurrence.end) == Some(&b"Tile"[..])
        })
        .map(|occurrence| occurrence.name.as_str())
        .collect();
    assert_eq!(
        tile_targets,
        vec![
            "Vendor\\Widget\\Card",
            "Vendor\\Widget\\Card",
            "Vendor\\Other\\Card"
        ]
    );

    let draw = occurrence(
        &extracted,
        source,
        b"draw",
        "call",
        "Vendor\\Format\\render",
    );
    assert_eq!(draw.provenance, "php_call");
    assert_eq!(draw.target, None);
    let shade = occurrence(
        &extracted,
        source,
        b"Shade",
        "read",
        "Vendor\\Config\\SHADE",
    );
    assert_eq!(source_span(source, shade.start, shade.end), b"Shade");
    let lowercase_shade = occurrence(&extracted, source, b"shade", "read", "App\\shade");
    assert_eq!(
        source_span(source, lowercase_shade.start, lowercase_shade.end),
        b"shade"
    );
}

#[test]
fn duplicate_import_aliases_remain_ambiguous() {
    let source = br#"<?php
namespace App;
use Vendor\One\Widget as Item;
use Vendor\Two\Widget as Item;
new Item();
"#;

    let extracted = extract(source);
    assert_eq!(extracted.status, "complete");
    let reference = occurrence(&extracted, source, b"Item", "type", "Item");
    assert_eq!(reference.provenance, "php_ambiguous_import");
    assert_eq!(reference.target, None);
    assert!(reference.candidates.is_empty());
}

#[test]
fn xenforo_placeholders_never_receive_local_class_candidates() {
    let source = b"<?php class xfcp_Impl {} new xfcp_Impl();";
    let extracted = extract(source);
    let placeholder = occurrence(&extracted, source, b"xfcp_Impl", "type", "xfcp_Impl");
    assert_eq!(placeholder.provenance, "xenforo_generated_placeholder");
    assert_eq!(placeholder.target, None);
    assert!(placeholder.candidates.is_empty());
}

#[test]
fn attribute_and_cast_operands_keep_expression_name_roles() {
    let source = br#"<?php
namespace App;
class Meta {}
class FLAG {}
class Inner {}
class Constants { public const VALUE = 1; }
const FLAG = 1;
#[Meta(FLAG, new Inner(), Constants::VALUE)]
class Subject {}
$cast = (int) FLAG;
"#;

    let extracted = extract(source);
    assert_eq!(extracted.status, "complete");
    occurrence(&extracted, source, b"Meta", "type", "App\\Meta");
    occurrence(&extracted, source, b"Inner", "type", "App\\Inner");
    occurrence(&extracted, source, b"Constants", "type", "App\\Constants");

    let flag_reads: Vec<_> = extracted
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == "read"
                && occurrence.name == "App\\FLAG"
                && source.get(occurrence.start..occurrence.end) == Some(&b"FLAG"[..])
        })
        .collect();
    assert_eq!(flag_reads.len(), 2);
    assert!(
        flag_reads
            .iter()
            .all(|read| read.provenance == "php_constant")
    );
    assert!(!extracted.occurrences.iter().any(|occurrence| {
        occurrence.role == "type"
            && source.get(occurrence.start..occurrence.end) == Some(&b"FLAG"[..])
    }));
}

#[test]
fn inheritance_trait_use_and_calls_remain_candidate_evidence() {
    let source = br#"<?php
namespace Acme;
class Base { public function inherited() {} }
interface Contract {}
trait AddsMethods {}
class Child extends Base implements Contract {
    use AddsMethods;
    public function run() { $this->inherited(); }
}
"#;

    let extracted = extract(source);
    assert_eq!(extracted.status, "complete");
    let base_index = extracted
        .definitions
        .iter()
        .position(|definition| definition.name == "Base" && definition.kind == "class")
        .expect("Base definition");
    let base_reference = occurrence(&extracted, source, b"Base", "type", "Acme\\Base");
    assert_eq!(base_reference.target, None);
    assert!(base_reference.candidates.contains(&base_index));
    assert_eq!(base_reference.provenance, "php_fqcn");

    let trait_index = extracted
        .definitions
        .iter()
        .position(|definition| definition.name == "AddsMethods" && definition.kind == "trait")
        .expect("AddsMethods definition");
    let trait_reference = occurrence(
        &extracted,
        source,
        b"AddsMethods",
        "type",
        "Acme\\AddsMethods",
    );
    assert_eq!(trait_reference.target, None);
    assert!(trait_reference.candidates.contains(&trait_index));

    let child_index = extracted
        .definitions
        .iter()
        .position(|definition| definition.name == "Child" && definition.kind == "class")
        .expect("Child definition");
    for kind in ["extends", "implements", "uses_trait"] {
        assert!(extracted.relationships.iter().any(|relationship| {
            relationship.source == child_index
                && relationship.kind == kind
                && relationship.target.is_none()
        }));
    }

    let call = extracted
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == "call"
                && source.get(occurrence.start..occurrence.end) == Some(&b"inherited"[..])
        })
        .expect("method call occurrence");
    assert_eq!(call.provenance, "php_call");
    assert_eq!(call.target, None);
}

#[test]
fn invalid_encoding_and_zero_budget_return_explicit_statuses() {
    let invalid = extract(&[0xff, 0xfe]);
    assert_eq!(invalid.status, "invalid_encoding");
    assert!(invalid.definitions.is_empty());

    let cancelled = super::extract_with_deadline(b"<?php class NeverParsed {}", Duration::ZERO);
    assert_eq!(cancelled.status, "cancelled");
    assert!(cancelled.definitions.is_empty());
}

#[test]
fn global_declaration_keeps_parameter_reads_unresolved() {
    let source = b"<?php function uncertain($param) { global $param; return $param; }";
    let extracted = extract(source);
    let read = extracted
        .occurrences
        .iter()
        .rev()
        .find(|occurrence| occurrence.role == "read" && occurrence.name == "$param")
        .expect("read after global declaration");
    assert_eq!(read.target, None);
}
