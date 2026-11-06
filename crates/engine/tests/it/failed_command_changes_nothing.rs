//! A command that fails part way changes nothing: the drawing and the selection are as they were
//! before it, and no undo step is recorded ("undo the call").

use std::sync::Arc;

use cadcraft_engine::Session;
use serde_json::json;

#[test]
fn a_command_that_fails_after_a_change_keeps_none_of_it() {
    let mut s = Session::new();
    let h = s.execute("circle", &json!({"center": [0, 0], "radius": 1})).unwrap()["handle"].as_str().unwrap().to_string();
    let (before, selection, undo) = {
        let st = s.state().unwrap();
        (st.doc.clone(), st.selection.clone(), st.undo.len())
    };
    // PROPERTIES.SET makes the layer it is given before it finds the dimension style missing.
    let err = s.execute("properties.set", &json!({"handles": [h], "layer": "Walls", "dimStyle": "Nope"})).unwrap_err();
    assert!(err.to_string().contains("Nope"), "{err}");
    let st = s.state().unwrap();
    assert!(st.doc.layer("Walls").is_none(), "the layer made before the failure is gone");
    assert!(Arc::ptr_eq(&st.doc, &before), "the drawing is the one from before the call");
    assert_eq!(st.undo.len(), undo, "a failed command records no undo step");
    assert_eq!(st.selection, selection);
    // The next undo takes back the circle, not an empty step left by the failed call.
    assert_eq!(s.undo().unwrap().as_deref(), Some("Circle"));
}
