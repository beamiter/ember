//! Protocol assertions use the vendored AT-SPI XML and the same zbus version as production.
use zbus::{Signature, Type};
use zbus_xml::{ArgDirection, Node};

fn documents() -> impl Iterator<Item = Node<'static>> {
	std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/xml"))
		.unwrap()
		.map(Result::unwrap)
		.filter(|entry| entry.path().extension().is_some_and(|ext| ext == "xml"))
		.map(|entry| Node::from_reader(std::fs::File::open(entry.path()).unwrap()).unwrap())
}

pub(crate) fn method(
	member: &str,
	interface: Option<&str>,
	argument: Option<&str>,
	output: bool,
) -> Signature {
	let mut signatures = Vec::new();
	for node in documents() {
		for iface in node.interfaces() {
			if interface.is_some_and(|name| iface.name().as_str() != name) {
				continue;
			}
			for method in iface.methods().filter(|method| method.name().as_str() == member) {
				let args: Vec<_> = method
					.args()
					.iter()
					.filter(|arg| match argument {
						Some(name) => arg.name() == Some(name),
						None => {
							arg.direction().unwrap_or(ArgDirection::In)
								== if output { ArgDirection::Out } else { ArgDirection::In }
						}
					})
					.collect();
				let signature = args.iter().map(|arg| arg.ty().to_string()).collect::<String>();
				signatures.push(signature);
			}
		}
	}
	assert_eq!(signatures.len(), 1, "Expected exactly one XML method {interface:?}.{member}");
	signatures.pop().unwrap().parse().unwrap()
}

pub(crate) fn signal(member: &str) -> Signature {
	let mut signatures = Vec::new();
	for node in documents() {
		for iface in node.interfaces() {
			for signal in iface.signals().filter(|signal| signal.name().as_str() == member) {
				signatures.push(
					signal
						.args()
						.iter()
						.map(|arg| arg.ty().to_string())
						.collect::<String>(),
				);
			}
		}
	}
	assert!(!signatures.is_empty(), "Missing XML signal {member}");
	assert!(signatures.iter().all(|s| s == &signatures[0]), "Ambiguous XML signal {member}");
	signatures[0].parse().unwrap()
}

#[test]
fn former_lockstep_type_validations() {
	assert_eq!(crate::ObjectRef::SIGNATURE, &signal("Available"));
	assert_eq!(crate::ObjectRefOwned::SIGNATURE, &signal("Available"));
	assert_eq!(crate::CacheItem::SIGNATURE, &signal("AddAccessible"));
	// Multiple signal arguments form a structure when used as a Rust body type.
	assert_eq!(
		crate::events::EventBodyOwned::SIGNATURE.to_string(),
		format!("({})", signal("PropertyChange").to_string_no_parens())
	);
	assert_eq!(
		crate::events::registry::EventListeners::SIGNATURE.to_string(),
		format!("({})", signal("EventListenerRegistered").to_string_no_parens())
	);
}

#[test]
fn accessible_method_and_parent_signatures() {
	let accessible = "org.a11y.atspi.Accessible";
	assert_eq!(crate::StateSet::SIGNATURE, &method("GetState", Some(accessible), None, true));
	assert_eq!(
		crate::InterfaceSet::SIGNATURE,
		&method("GetInterfaces", Some(accessible), None, true)
	);
	assert_eq!(crate::Role::SIGNATURE, &method("GetRole", Some(accessible), None, true));
	let xml = Node::try_from(include_str!("../xml/Accessible.xml")).unwrap();
	let interface = xml
		.interfaces()
		.iter()
		.find(|i| i.name().as_str() == accessible)
		.unwrap();
	let parent = interface
		.properties()
		.find(|p| p.name().as_str() == "Parent")
		.unwrap();
	assert_eq!(crate::ObjectRefOwned::SIGNATURE, parent.ty());
	assert_eq!(crate::ObjectRefOwned::SIGNATURE.to_string(), "(so)");
}
