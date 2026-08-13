use super::*;

#[test]
fn anchor_contract_preserves_scale_legacy_focus_and_unique_identity() {
    let anchor =
        Anchor::logical("blade", [1.0, 2.0, 3.0, 4.0], 1.5).expect("forge physical anchor");
    assert_eq!(anchor.rect, [1.5, 3.0, 4.5, 6.0]);
    assert!(!anchor.focused);

    let legacy: Anchor =
        serde_json::from_str(r#"{"name":"blade","rect":[0,0,1,1]}"#).expect("decode legacy anchor");
    assert!(!legacy.focused);

    let anchors = [
        Anchor::physical("blade", [0.0, 0.0, 1.0, 1.0]).expect("first anchor"),
        Anchor::physical("blade", [1.0, 1.0, 2.0, 2.0]).expect("second anchor"),
    ];
    assert!(PendingFrame::forge(1, 1.0, anchors, ()).is_err());
}

#[test]
fn frame_journal_round_trips_complete_records() {
    let temporary = tempfile::NamedTempFile::new().expect("temporary journal");
    let mut output = open_frame_journal(temporary.path(), "launch").expect("journal header");
    let first = FrameSample {
        frame: 4,
        surface_sequence: 1,
        begun_ns: 10,
        observed_ns: 20,
        surface_presented_ns: 30,
    };
    let second = FrameSample {
        frame: 5,
        surface_sequence: 2,
        begun_ns: 40,
        observed_ns: 50,
        surface_presented_ns: 60,
    };
    append_frame(&mut output, temporary.path(), first).expect("first frame");
    append_frame(&mut output, temporary.path(), second).expect("second frame");
    output.write_all(&[0xAA, 0xBB]).expect("partial tail");
    output.flush().expect("flush journal");
    assert_eq!(
        read_frame_journal(temporary.path(), "launch").expect("read journal"),
        vec![first, second]
    );
}

#[test]
fn observation_journal_retains_brief_and_partial_records() {
    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    struct Mark {
        value: u8,
    }

    let root = tempfile::tempdir().expect("temporary journal root");
    let path = root.path().join("witness.observations");
    let mut output = open_observation_journal(&path, "launch").expect("journal header");
    let first = serde_json::to_vec(&Mark { value: 1 }).expect("first record");
    append_observation(&mut output, &path, &first).expect("append first record");
    let second = serde_json::to_vec(&Mark { value: 2 }).expect("second record");
    let length = u32::try_from(second.len())
        .expect("tiny record length")
        .to_le_bytes();
    output.write_all(&length).expect("partial record length");
    output.write_all(&second[..2]).expect("partial record body");

    let mut reader = ObservationJournal::sealed(&path, "launch");
    assert_eq!(
        reader.read_new::<Mark>().expect("first read"),
        vec![Mark { value: 1 }]
    );
    assert!(
        reader
            .read_new::<Mark>()
            .expect("partial tail is not corruption")
            .is_empty()
    );

    output
        .write_all(&second[2..])
        .expect("complete second record");
    assert_eq!(
        reader.read_new::<Mark>().expect("second read"),
        vec![Mark { value: 2 }]
    );
}

#[test]
fn publisher_drop_drains_both_journals_in_surface_order() {
    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    struct Mark {
        value: u8,
    }

    let root = tempfile::tempdir().expect("publisher root");
    let observations = root.path().join("observations");
    let frames = root.path().join("frames");
    let mut publisher = Publisher::raise(observations.clone(), frames.clone(), "launch".to_owned())
        .expect("raise publisher");
    for (frame, value) in [(7, 1), (7, 2)] {
        let begun = ProductInstant(u64::from(value) * 10);
        let observed = ProductInstant(begun.0 + 1);
        let pending = PendingFrame::forge_at(
            FrameObservation::from_instants(begun, observed).expect("ordered observation"),
            frame,
            1.0,
            [],
            Mark { value },
        )
        .expect("stage frame");
        let _sequence = publisher
            .surface_present_at(pending, ProductInstant(observed.0 + 1))
            .expect("enqueue frame");
    }
    drop(publisher);

    let mut journal = ObservationJournal::sealed(&observations, "launch");
    assert_eq!(
        journal
            .read_new::<WireFrameOwned<Mark>>()
            .expect("read observations"),
        [
            WireFrameOwned {
                surface_sequence: 1,
                state: Mark { value: 1 },
            },
            WireFrameOwned {
                surface_sequence: 2,
                state: Mark { value: 2 },
            },
        ]
    );
    let samples = read_frame_journal(&frames, "launch").expect("read frame journal");
    assert_eq!(
        samples
            .iter()
            .map(|sample| sample.surface_sequence)
            .collect::<Vec<_>>(),
        [1, 2]
    );
}

#[derive(Debug, Deserialize, PartialEq)]
struct WireFrameOwned<T> {
    surface_sequence: u64,
    state: T,
}

#[cfg(feature = "egui")]
#[test]
fn final_egui_pass_projects_only_presented_targets_and_focus() {
    use ::egui::{Context, RawInput, Rect, pos2};

    let ctx = Context::default();
    egui::install(&ctx);
    let mut pass = 0;
    ctx.run_ui(RawInput::default(), |ui| {
        pass += 1;
        if pass == 1 {
            egui::record_rect(
                ui.ctx(),
                "discarded",
                Rect::from_min_max(pos2(1.0, 0.0), pos2(2.0, 1.0)),
            );
            ui.ctx().request_discard("exercise final-pass telemetry");
        }
        let blade = ui.button("blade");
        blade.request_focus();
        egui::record_response(ui, "blade", &blade);
    })
    .drop_without_applying_deltas();
    let anchors = egui::take(&ctx, 1.0).expect("focused response anchors");
    assert_eq!(pass, 2);
    let [blade] = anchors.as_slice() else {
        panic!("expected only the final-pass blade, found {anchors:?}");
    };
    assert_eq!(blade.name, "blade");
    assert!(blade.focused);
}
