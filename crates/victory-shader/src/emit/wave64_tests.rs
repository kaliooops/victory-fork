//! A console wave64 compute program on a host of 32-lane waves: a wave reduction inside a
//! per-lane branch, with that branch taken wave-wide ([`crate::structure::structure_wide`])
//! so that the two host waves can exchange their halves inside it. Lowered, validated by
//! `dxil.dll` when present, and run on the host GPU when it takes waves of 32.

use std::collections::HashSet;

use super::host::{Dispatch, Host, Root, View};
use super::{HostShader, lower, validator};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{ShaderError, io, ir, resource, structure};

const S_ENDPGM: u32 = 0xBF81_0000;

/// 64 threads in one console wave. Each thread t holds the bit `1 << (t >> 1)`; the threads
/// below s0 (a root constant) OR theirs over the wave inside `if (t < s0) { ... }` and store
/// the result to dword 0 of the raw UAV.
/// `skip`: the branch past the body when no thread is below s0: `s_cbranch_execz`, or
/// `s_cbranch_scc0` on the SCC `s_and_saveexec` sets ("some lane is left").
fn program(skip: u32) -> NativeProgram {
    let mut words = vec![
        0x7E0A_0281, // v_mov_b32 v5, 1
        0x2C0C_0081, // v_lshrrev_b32 v6, 1, v0
        0x340E_0B06, // v_lshlrev_b32 v7, v6, v5
        0x7D88_0000, // v_cmp_gt_u32 vcc, s0, v0
        0xBE8C_246A, // s_and_saveexec_b64 s[12:13], vcc
        skip | 23,   // the branch to the restore of EXEC
    ];
    words.extend_from_slice(&super::wave::WAVE_OR_V7_TO_S7);
    words.extend_from_slice(&[
        0x7E00_0207, // v_mov_b32 v0, s7
        0x38 << 26 | 0x1C << 18,
        0x80 << 24 | 2 << 16, // buffer_store_dword v0, off, s[8:11], 0
        0xBEFE_040C, // s_mov_b64 exec, s[12:13]
    ]);
    native(&words)
}

/// A wave64 compute program of 64 threads (one console wave; the thread index in v0) with 4
/// root constants in s0..s3 and a raw UAV's descriptor in s[8:11], then `body`.
fn native(body: &[u32]) -> NativeProgram {
    native_of(body, true, 64)
}

/// The same for a console of 32-lane waves when `wave64` is not set, with `threads` threads.
fn native_of(body: &[u32], wave64: bool, threads: u32) -> NativeProgram {
    let mut words = vec![
        0xBE85_0380, // s_mov_b32 s5, 0
        0x3D << 26 | 0x02 << 18 | 8 << 6 | 2,
        125 << 25, // s_load_dwordx4 s[8:11], s[4:5], 0
    ];
    words.extend_from_slice(body);
    words.push(S_ENDPGM);
    NativeProgram {
        stage: HwStage::Cs,
        wave64,
        vgprs: 16,
        layout: vec![
            RootParam {
                root: RootType::Constants,
                table_offset_dwords: 0,
                sgprs: 4,
                base_register: 0,
                user_data: 0,
                slot: 0,
            },
            RootParam {
                root: RootType::Table,
                table_offset_dwords: 0,
                sgprs: 1,
                base_register: 0,
                user_data: 4,
                slot: 1,
            },
        ],
        used: vec![UsedResource {
            kind: ResourceType::Uav,
            root: RootType::Table,
            table_offset_dwords: 0,
            lower_bound: 0,
            stride: 0,
            dwords: 4,
            user_data: 4,
            slot: 1,
        }],
        io: StageIo::Cs(ComputeIo {
            rsrc2: 5 << 1, // 5 user SGPRs: s0..s4
            threads: [threads, 1, 1],
        }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    }
}

/// `program()` for a host of 32-lane waves, with the branches closing `wide` taken wave-wide.
fn lowered(native: &NativeProgram, p: &ir::Program, wide: &HashSet<usize>) -> Result<HostShader, ShaderError> {
    let st = structure::structure_wide(p, wide).expect("structure");
    let res = resource::track(p, &native.layout, native.stage, Some(&st)).expect("resource::track");
    let io = io::raise(p, native).expect("io::raise");
    let was = lower::TEST_HOST_WAVE.replace(Some(32));
    let lowered = lower::lower(&lower::Parts {
        native,
        p,
        st: &st,
        io: &io,
        res: &res,
        inputs: Vec::new(),
        outputs: Vec::new(),
        prims: Vec::new(),
        mesh_stride: 0,
    });
    lower::TEST_HOST_WAVE.set(was);
    let lowered = lowered?;
    Ok(HostShader {
        stage: HwStage::Cs,
        dxil: lowered.dxil,
        user_data: lowered.user_data,
        samplers: lowered.samplers,
        static_samplers: Vec::new(),
        root_cbvs: lowered.root_cbvs,
        root_srvs: lowered.root_srvs,
        root_uavs: lowered.root_uavs,
        root_indexed_views: res.root_indexed_views,
    })
}

/// All lanes initialize their LDS slot. Only t < s0 swaps with the other
/// half of its console wave and adds a lane-32 read. Inactive console waves
/// must meet every inserted barrier without writing either LDS or the UAV.
#[test]
fn multi_wave_groups_keep_inactive_lanes_predicated_across_lds_and_lane_reads() {
    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let host = Host::new(false).expect("SM 6.6 hardware device");
    for threads in [128u32, 256] {
        let mut words = vec![
            0x340C_0082, // v_lshlrev_b32 v6, 2, v0
            0x4A0A_00FF,
            100, // v_add_nc_u32 v5, 100, v0
            0xD834_0000,
            0x0000_0506,            // ds_write_b32 v6, v5
            0xBF8A_0000,            // s_barrier
            29 << 25 | 4 << 17 | 1, // v_xor_b32 v4, s1, v0
            0x7D88_0800,            // v_cmp_gt_u32 vcc, s0, v4
            0xBE8C_246A,            // s_and_saveexec_b64 s[12:13], vcc
            0xBF88_0000,            // s_cbranch_execz restore
        ];
        let skip = words.len() - 1;
        words.extend_from_slice(&[
            29 << 25 | 2 << 17 | 0xA0, // v_xor_b32 v2, 32, v0
            0x3404_0482,               // v_lshlrev_b32 v2, 2, v2
            0xD8D8_0000,
            0x0700_0002, // ds_read_b32 v7, v2
            0xD760_0014,
            0x0001_4105,                      // v_readlane_b32 s20, v5, 32
            37 << 25 | 7 << 17 | 7 << 9 | 20, // v_add_nc_u32 v7, s20, v7
            0xD834_0000,
            0x0000_0706, // ds_write_b32 v6, v7
        ]);
        words[skip] |= (words.len() - skip - 1) as u32;
        words.extend_from_slice(&[
            0xBEFE_040C, // s_mov_b64 exec, s[12:13]
            0xBF8A_0000, // s_barrier
            0xD8D8_0000,
            0x0700_0006, // ds_read_b32 v7, v6
            0xE070_1000,
            0x8002_0706, // buffer_store_dword v7, v6, s[8:11], 0 offen
        ]);
        let mut native = native_of(&words, true, threads);
        if let StageIo::Cs(cs) = &mut native.io {
            cs.rsrc2 |= 2 << 15;
        }
        let p = ir::build(&native.code, true, native.vgprs).expect("IR");
        let branch = p
            .blocks
            .iter()
            .position(|b| {
                b.insts
                    .last()
                    .is_some_and(|i| i.name() == "s_cbranch_execz")
            })
            .unwrap();
        assert!(super::group_wide_skip_or_loop(&p, branch));
        let shader = lowered(&native, &p, &HashSet::from([branch])).expect("group-wide lowering");
        let mut validator =
            validator::Validator::load(validator::Validator::default_path().as_deref())
                .expect("DXIL validator");
        validator
            .validate(&mut shader.dxil.clone())
            .expect("valid DXIL");
        let (root, pso) = host.compute_pipeline(&shader).expect("WaveSize32 pipeline");
        for (below, flip) in [
            (0, 0),
            (1, 0),
            (31, 0),
            (32, 0),
            (33, 0),
            (63, 0),
            (64, 0),
            (65, 0),
            (127, 0),
            (threads, 0),
            (1, 64),
            (33, 64),
            (1, 128),
            (65, 128),
        ] {
            let mut ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
            for u in &shader.user_data {
                if (u.root, u.slot, u.dword) == (RootType::Constants, 0, 0) {
                    ud[u.index as usize] = below;
                }
                if (u.root, u.slot, u.dword) == (RootType::Constants, 0, 1) {
                    ud[u.index as usize] = flip;
                }
            }
            let out = host
                .dispatch(
                    &root,
                    &pso,
                    &Dispatch {
                        roots: vec![(0, Root::Constants(ud))],
                        views: &[View::RawUav(threads)],
                        samplers: &[],
                        groups: [1, 1, 1],
                    },
                )
                .expect("multi-wave dispatch");
            for t in 0..threads {
                let expected = if t ^ flip < below {
                    (t ^ 32) + 100 + (t / 64 * 64 + 132)
                } else {
                    t + 100
                };
                assert_eq!(
                    out[0][t as usize], expected,
                    "group={threads} active={below} thread={t}"
                );
            }
        }
    }
}

#[test]
fn multi_wave_masked_loops_wait_for_the_last_console_wave() {
    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let host = Host::new(false).expect("SM 6.6 hardware device");
    for threads in [128u32, 256] {
        let mut words = vec![
            0x340C_0082, // v_lshlrev_b32 v6, 2, v0
            0x7E0A_0300, // v_mov_b32 v5, v0: each lane starts at a different iteration
            0x7E0E_0280, // v_mov_b32 v7, 0
            0x7D88_0A00, // v_cmp_gt_u32 vcc, s0, v5
            0xBE8C_246A, // s_and_saveexec_b64 s[12:13], vcc
            0xBF88_0000, // s_cbranch_execz restore
        ];
        let skip = words.len() - 1;
        let body = words.len();
        words.extend_from_slice(&[
            0xD834_0000,
            0x0000_0706, // ds_write_b32 v6, v7
            0xD8D8_0000,
            0x0200_0006,                       // ds_read_b32 v2, v6
            37 << 25 | 7 << 17 | 2 << 9 | 129, // v_add_nc_u32 v7, 1, v2
            37 << 25 | 5 << 17 | 5 << 9 | 129, // v_add_nc_u32 v5, 1, v5
            0x7D88_0A00,                       // v_cmp_gt_u32 vcc, s0, v5
            0x87FE_6A7E,                       // s_and_b64 exec, exec, vcc
            0xBF89_0000,                       // s_cbranch_execnz body
        ]);
        let back = words.len() - 1;
        words[back] |= ((body as i32 - back as i32 - 1) as i16 as u16) as u32;
        words[skip] |= (words.len() - skip - 1) as u32;
        words.extend_from_slice(&[
            0xBEFE_040C, // restore EXEC, including the lanes that never entered
            0xE070_1000,
            0x8002_0706, // buffer_store_dword v7, v6, s[8:11], 0 offen
        ]);
        let mut native = native_of(&words, true, threads);
        if let StageIo::Cs(cs) = &mut native.io {
            cs.rsrc2 |= 2 << 15;
        }
        let p = ir::build(&native.code, true, native.vgprs).expect("IR");
        let wide: HashSet<usize> = p
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(b, block)| matches!(block.exit, ir::Exit::Branch { .. }).then_some(b))
            .collect();
        assert!(wide.iter().all(|&b| super::group_wide_skip_or_loop(&p, b)));
        let shader = lowered(&native, &p, &wide).expect("masked loop lowering");
        let mut validator =
            validator::Validator::load(validator::Validator::default_path().as_deref())
                .expect("DXIL validator");
        validator
            .validate(&mut shader.dxil.clone())
            .expect("valid DXIL");
        let (root, pso) = host.compute_pipeline(&shader).expect("WaveSize32 pipeline");
        for iterations in [0u32, 1, 2, 33, 65, 129] {
            let mut ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
            for u in &shader.user_data {
                if (u.root, u.slot, u.dword) == (RootType::Constants, 0, 0) {
                    ud[u.index as usize] = iterations;
                }
            }
            let out = host
                .dispatch(
                    &root,
                    &pso,
                    &Dispatch {
                        roots: vec![(0, Root::Constants(ud))],
                        views: &[View::RawUav(threads)],
                        samplers: &[],
                        groups: [1, 1, 1],
                    },
                )
                .expect("masked loop dispatch");
            for t in 0..threads {
                assert_eq!(
                    out[0][t as usize],
                    iterations.saturating_sub(t),
                    "group={threads} iterations={iterations} thread={t}"
                );
            }
        }
    }
}

#[test]
fn group_widening_does_not_mistake_a_diamond_for_an_exec_skip() {
    let native = native_of(
        &[
            0x7D88_0000, // v_cmp_gt_u32 vcc, s0, v0
            0xBE8C_246A, // s_and_saveexec_b64 s[12:13], vcc
            0xBF88_0002, // s_cbranch_execz else
            0x7E0A_0281, // v_mov_b32 v5, 1
            0xBF82_0001, // s_branch merge
            0x7E0A_0282, // else: v_mov_b32 v5, 2
            0xBEFE_040C, // merge: restore EXEC
            0xE070_0000,
            0x8002_0500, // buffer_store_dword v5, off, s[8:11], 0
        ],
        true,
        128,
    );
    let p = ir::build(&native.code, true, native.vgprs).expect("IR");
    let branch = p
        .blocks
        .iter()
        .position(|b| matches!(b.exit, ir::Exit::Branch { .. }))
        .unwrap();
    assert!(
        !super::group_wide_skip_or_loop(&p, branch),
        "both arms need serialization, not one group vote"
    );
}

#[test]
fn widening_keeps_descriptor_provenance_on_logical_lane_paths() {
    let mut words = vec![
        0xF40C_0402,
        0xFA00_0020, // load texture s[16:23] from table + 32
        0x7D88_0000, // v_cmp_gt_u32 vcc, s0, v0
        0xBE8C_246A, // mask EXEC to lanes below s0
        0xBF88_0000, // outer empty-wave exit, patched below
        0xBE8E_247E, // s_and_saveexec_b64 s[14:15], exec
        0xBF88_0009, // skip constant sampler and lane read when no lane is active
        0xB000_0092, // sampler dword 0
        0xBE81_03FF,
        0x00C0_0000,
        0xBE82_0BA8, // sampler dword 2: bit reverse of 40
        0xBE83_0380,
        0xD760_0018,
        0x0001_4100, // v_readlane_b32 s24, v0, 32
        0x7E04_0218, // v_mov_b32 v2, s24
        0x7E02_0280, // v_mov_b32 v1, 0
        0xBEFE_040E, // restore the active lane mask saved in s[14:15]
        0xF090_0108,
        0x0004_0700, // image_sample_l v7, v[0:2], s[16:23], s[0:3]
        0xE070_0000,
        0x8002_0700, // store v7
    ];
    words[4] |= (words.len() - 5) as u32;
    let mut native = native(&words);
    native.layout.push(RootParam {
        root: RootType::Table,
        table_offset_dwords: 8,
        sgprs: 1,
        base_register: 1,
        user_data: 4,
        slot: 1,
    });
    native.used.push(UsedResource {
        kind: ResourceType::Srv,
        root: RootType::Table,
        table_offset_dwords: 8,
        lower_bound: 1,
        stride: 0,
        dwords: 8,
        user_data: 4,
        slot: 1,
    });
    let p = ir::build(&native.code, true, native.vgprs).expect("IR");
    let cbytes = crate::dxbc::build(&[]);
    let c = crate::dxbc::Container::parse(&cbytes).unwrap();
    let was = lower::TEST_HOST_WAVE.replace(Some(32));
    let result = super::widen(&c, &native, &p, false);
    lower::TEST_HOST_WAVE.set(was);
    let (shader, wave) = result.expect("logical descriptor remains traceable after widening");
    assert_eq!(wave, Some(32));
    let mut validator = validator::Validator::load(validator::Validator::default_path().as_deref())
        .expect("DXIL validator");
    validator
        .validate(&mut shader.dxil.clone())
        .expect("valid DXIL");
}

#[test]
fn a_sum_of_halves_over_the_wave_is_the_consoles_tree_bit_for_bit() {
    // thread t holds the 16-bit float with bits 0x3C00 + 37 t (1.0 and up, with odd
    // mantissas, so that additions round); all of them store the wave's sum
    let mut words = vec![
        0x340C_0085, // v_lshlrev_b32 v6, 5, v0
        0x340E_0082, // v_lshlrev_b32 v7, 2, v0
        0x4A0E_0F06, // v_add_nc_u32 v7, v6, v7
        0x4A0E_0F00, // v_add_nc_u32 v7, v0, v7
        0x4A0E_0EFF,
        0x0000_3C00, // v_add_nc_u32 v7, 0x3c00, v7
    ];
    // (the wave OR's words with v_add_f16 for v_or_b32: opcode 50 for 28)
    words.extend(super::wave::WAVE_OR_V7_TO_S7.iter().map(|&w| if w >> 25 == 28 { w & 0x01FF_FFFF | 50 << 25 } else { w }));
    words.extend_from_slice(&[
        0x7E00_0207, // v_mov_b32 v0, s7
        0x38 << 26 | 0x1C << 18,
        0x80 << 24 | 2 << 16, // buffer_store_dword v0, off, s[8:11], 0
    ]);
    let native = native(&words);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    assert_eq!(p.blocks.iter().flat_map(|b| &b.insts).filter(|i| i.name() == "v_add_f16").count(), 6);
    let shader = lowered(&native, &p, &HashSet::new()).expect("lower");
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        let mut copy = shader.dxil.clone();
        v.validate(&mut copy).expect("DXIL validation failed");
    }
    // the console's sum: lanes 2k and 2k+1, then pairs, ... each addition rounded to 16 bits
    // (to nearest, ties to even); values here are normal numbers
    let wide = |h: u16| f32::from_bits((u32::from(h >> 10) + 112) << 23 | u32::from(h & 0x3FF) << 13);
    let narrow = |f: f32| {
        let b = f.to_bits();
        let h = ((b >> 23) - 112) << 10 | (b >> 13 & 0x3FF);
        let rest = b & 0x1FFF;
        (h + u32::from(rest > 0x1000 || rest == 0x1000 && h & 1 == 1)) as u16
    };
    let add = |a: u16, b: u16| narrow(wide(a) + wide(b));
    let terms: Vec<u16> = (0..64u16).map(|t| 0x3C00 + 37 * t).collect();
    let mut tree = terms.clone();
    while tree.len() > 1 {
        tree = tree.chunks(2).map(|c| add(c[0], c[1])).collect();
    }
    let in_a_row = terms.iter().copied().reduce(add).unwrap();
    assert_ne!(tree[0], in_a_row, "these terms tell the tree from another order");
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Ok(host) = Host::new(false) else {
        eprintln!("no SM 6.6 hardware device; not run");
        return;
    };
    let (root, pso) = match host.compute_pipeline(&shader) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("the hardware device does not take this program (waves of 32?): {e}; not run");
            return;
        }
    };
    let ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
    let out = host
        .dispatch(
            &root,
            &pso,
            &Dispatch {
                roots: vec![(0, Root::Constants(ud))],
                views: &[View::RawUav(1)],
                samplers: &[],
                groups: [1, 1, 1],
            },
        )
        .unwrap_or_else(|e| panic!("dispatch failed: {e}"));
    // (the high half is lane 0's term's: zero)
    assert_eq!(out[0][0], u32::from(tree[0]), "tree {:#x}, in a row {in_a_row:#x}", tree[0]);
    eprintln!("ran on the hardware device: the sum {:#x} of the console's tree", tree[0]);
}

/// One `v_add_f16` whose exact result lies between two halves: 1 + 3/1024 (0x3C03) plus 2
/// (0x4000) is 3 + 1.5 steps of that range, which the console rounds up to 0x4202. The
/// host's `f32tof16` may give 0x4201 (toward zero); with `VICTORY_F16_NEAREST=1` the
/// translation gives the console's whatever the host does.
#[test]
fn a_16_bit_sum_rounds_to_nearest_when_asked_to() {
    let words = [
        0x7E0E_02FF,
        0x0000_3C03, // v_mov_b32 v7, 0x3c03
        0x7E0C_02FF,
        0x0000_4000, // v_mov_b32 v6, 0x4000
        0x6400_0D07, // v_add_f16 v0, v7, v6
        0x38 << 26 | 0x1C << 18,
        0x80 << 24 | 2 << 16, // buffer_store_dword v0, off, s[8:11], 0
    ];
    let native = native(&words);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    assert!(p.blocks.iter().flat_map(|b| &b.insts).any(|i| i.name() == "v_add_f16"));
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    for nearest in [false, true] {
        let was = lower::TEST_F16_NEAREST.replace(Some(nearest));
        let shader = lowered(&native, &p, &HashSet::new());
        lower::TEST_F16_NEAREST.set(was);
        let shader = shader.expect("lower");
        if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
            let mut copy = shader.dxil.clone();
            v.validate(&mut copy).expect("DXIL validation failed");
        }
        let Ok(host) = Host::new(false) else {
            eprintln!("no SM 6.6 hardware device; not run");
            return;
        };
        let Ok((root, pso)) = host.compute_pipeline(&shader) else {
            eprintln!("the hardware device does not take this program; not run");
            return;
        };
        let ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
        let out = host
            .dispatch(
                &root,
                &pso,
                &Dispatch {
                    roots: vec![(0, Root::Constants(ud))],
                    views: &[View::RawUav(1)],
                    samplers: &[],
                    groups: [1, 1, 1],
                },
            )
            .unwrap_or_else(|e| panic!("dispatch failed: {e}"));
        let got = out[0][0] & 0xFFFF;
        eprintln!("v_add_f16 0x3c03 + 0x4000 on the hardware device, nearest {nearest}: {got:#x}");
        if nearest {
            assert_eq!(got, 0x4202);
        } else {
            assert!(got == 0x4201 || got == 0x4202, "{got:#x}");
        }
    }
}

#[test]
fn a_wave_or_inside_a_lane_branch_needs_the_branch_wave_wide() {
    for (skip, name) in [(0xBF88_0000, "s_cbranch_execz"), (0xBF84_0000, "s_cbranch_scc0")] {
        a_wave_or_inside(skip, name);
    }
}

fn a_wave_or_inside(skip: u32, name: &str) {
    let native = program(skip);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let branch = (0..p.blocks.len())
        .find(|&b| p.blocks[b].insts.last().is_some_and(|i| i.name() == name))
        .expect("the branch");
    // per lane, the two host waves cannot meet inside the branch
    lower::take_needs();
    let per_lane = lowered(&native, &p, &HashSet::new());
    assert!(
        matches!(&per_lane, Err(ShaderError::Unsupported { what, .. }) if *what == "a wave reduction where not every lane runs"),
        "{:?}",
        per_lane.err()
    );
    // ... and the lowering names the block, around which that branch is the one to widen
    let needs = lower::take_needs();
    let st = structure::structure(&p).expect("structure");
    assert_eq!(lower::branches_around(&st, &needs), Some(vec![branch]));
    // (taken, past the body, when every lane's bit is clear)
    assert_eq!(st.wave_any[branch], Some(false));
    // wave-wide, every lane runs the reduction (EXEC stays a guard on what it does there)
    let wide = HashSet::from([branch]);
    let st = structure::structure_wide(&p, &wide).expect("structure");
    let inside = match p.blocks[branch].exit {
        ir::Exit::Branch { not_taken, .. } => not_taken,
        _ => unreachable!(),
    };
    assert!(st.guards[inside].iter().any(Option::is_some));
    let shader = lowered(&native, &p, &wide).expect("lower");
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        let mut copy = shader.dxil.clone();
        v.validate(&mut copy).expect("DXIL validation failed");
    }
    // On a GPU that takes waves of 32 (two host waves for the console's one): the OR is of
    // both halves, whichever half's lanes take the branch.
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Ok(host) = Host::new(false) else {
        eprintln!("no SM 6.6 hardware device; not run");
        return;
    };
    let (root, pso) = match host.compute_pipeline(&shader) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("the hardware device does not take this program (waves of 32?): {e}; not run");
            return;
        }
    };
    // (threads below s0 take the branch; thread t gives bit t >> 1)
    for (below, expected) in [(64u32, u32::MAX), (40, 0x000F_FFFF), (33, 0x0001_FFFF), (10, 0x1F), (1, 1)] {
        let mut ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
        for u in &shader.user_data {
            if let (RootType::Constants, 0, 0) = (u.root, u.slot, u.dword) {
                ud[u.index as usize] = below;
            }
        }
        let out = host
            .dispatch(
                &root,
                &pso,
                &Dispatch {
                    roots: vec![(0, Root::Constants(ud))],
                    views: &[View::RawUav(1)],
                    samplers: &[],
                    groups: [1, 1, 1],
                },
            )
            .unwrap_or_else(|e| panic!("dispatch failed: {e}"));
        assert_eq!(out[0][0], expected, "{name}: {below} threads in the branch");
    }
    eprintln!("ran on the hardware device ({name}): the OR of both halves in every case");
}

/// Runs `shader` on the hardware device with the root constants `s0`, `s1` and gives dword 0
/// of the UAV; None (and a line) without a device that takes it.
fn on_the_card(shader: &HostShader, constants: [u32; 2]) -> Option<u32> {
    let Ok(host) = Host::new(false) else {
        eprintln!("no SM 6.6 hardware device; not run");
        return None;
    };
    let Ok((root, pso)) = host.compute_pipeline(shader) else {
        eprintln!("the hardware device does not take this program; not run");
        return None;
    };
    let mut ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
    for u in &shader.user_data {
        if let (RootType::Constants, 0, d @ 0..=1) = (u.root, u.slot, u.dword) {
            ud[u.index as usize] = constants[d as usize];
        }
    }
    let out = host
        .dispatch(
            &root,
            &pso,
            &Dispatch {
                roots: vec![(0, Root::Constants(ud))],
                views: &[View::RawUav(1)],
                samplers: &[],
                groups: [1, 1, 1],
            },
        )
        .unwrap_or_else(|e| panic!("dispatch failed: {e}"));
    Some(out[0][0])
}

/// A 32-lane console wave. A register holds a lane mask on one path (the lanes below s0)
/// and a number on the other, and is read as a number after the join, as the trace programs
/// keep their lanes' masks and their node numbers on one stack
/// (`deferred_tiled:CS_LaserLightShadowRaytracing`, s46 at 0x104, 0x75c and 0x188). The
/// mask arrives as the ballot of its lanes.
#[test]
fn a_lane_mask_joined_with_a_number_is_read_as_the_number_it_is() {
    let words = [
        0x7D88_0000, // v_cmp_gt_u32 vcc_lo, s0, v0
        0xBE94_036A, // s_mov_b32 s20, vcc_lo
        0xBF06_8001, // s_cmp_eq_u32 s1, 0
        0xBF85_0001, // s_cbranch_scc1 over the next
        0xBE94_0385, // s_mov_b32 s20, 5
        0x7E00_0214, // v_mov_b32 v0, s20
        0x38 << 26 | 0x1C << 18,
        0x80 << 24 | 2 << 16, // buffer_store_dword v0, off, s[8:11], 0
    ];
    let native = native_of(&words, false, 32);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let shader = lowered(&native, &p, &HashSet::new()).expect("lower");
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        let mut copy = shader.dxil.clone();
        v.validate(&mut copy).expect("DXIL validation failed");
    }
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    for (below, other, expected) in [(32u32, 0u32, u32::MAX), (10, 0, 0x3FF), (0, 0, 0), (10, 1, 5)] {
        let Some(got) = on_the_card(&shader, [below, other]) else { return };
        assert_eq!(got, expected, "{below} lanes in the mask, s1 = {other}");
    }
    eprintln!("ran on the hardware device: the mask's bits, or the number");
}

/// A 32-lane console wave. `if (t < s0) { ... SCC = (s1 == 7) }` and a branch on SCC after
/// the join: the skip brings the SCC of `s_and_saveexec` ("some lane is left") along
/// (`rtdirectionalshadows:CS_RTShadowTrace32HQ_TiledFarOnly`, 0x360-0x39c). On the console
/// the skip is taken by the wave, so with some lane inside every lane sees the inner test.
/// A lane that skipped on its own would see "some lane is left" instead: refused per lane,
/// right with the branch taken wave-wide. Lane 31 stores what it saw.
#[test]
fn scc_of_a_mask_operation_joined_with_a_scalar_test_needs_the_skip_wave_wide() {
    let words = [
        0x7D88_0000, // v_cmp_gt_u32 vcc_lo, s0, v0
        0xBE8D_3C6A, // s_and_saveexec_b32 s13, vcc_lo
        0xBF88_0001, // s_cbranch_execz over the next
        0xBF06_8701, // s_cmp_eq_u32 s1, 7
        0xBEFE_030D, // s_mov_b32 exec_lo, s13
        0xBF85_0002, // s_cbranch_scc1 to "seven"
        0x7E0A_0281, // v_mov_b32 v5, 1
        0xBF82_0001, // s_branch over the next
        0x7E0A_0282, // seven: v_mov_b32 v5, 2
        0x7DA4_009F, // v_cmpx_eq_u32 exec_lo, 31, v0
        0x38 << 26 | 0x1C << 18,
        0x80 << 24 | 2 << 16 | 5 << 8, // buffer_store_dword v5, off, s[8:11], 0
    ];
    let native = native_of(&words, false, 32);
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let branch = (0..p.blocks.len())
        .find(|&b| p.blocks[b].insts.last().is_some_and(|i| i.name() == "s_cbranch_execz"))
        .expect("the skip");
    lower::take_needs();
    let per_lane = lowered(&native, &p, &HashSet::new());
    assert!(
        matches!(&per_lane, Err(ShaderError::Unsupported { what, .. }) if *what == "a lane mask read as data"),
        "{:?}",
        per_lane.err()
    );
    let st = structure::structure(&p).expect("structure");
    assert_eq!(lower::branches_around(&st, &lower::take_needs()), Some(vec![branch]));
    let shader = lowered(&native, &p, &HashSet::from([branch])).expect("lower");
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        let mut copy = shader.dxil.clone();
        v.validate(&mut copy).expect("DXIL validation failed");
    }
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // (lanes below s0 inside, s1) -> what lane 31 stores: 2 if SCC was set at the branch
    for (below, s1, expected) in [(0u32, 7u32, 1u32), (10, 7, 2), (10, 3, 1), (32, 3, 1), (32, 7, 2)] {
        let Some(got) = on_the_card(&shader, [below, s1]) else { return };
        assert_eq!(got, expected, "{below} lanes inside, s1 = {s1}");
    }
    eprintln!("ran on the hardware device: the inner test's SCC for every lane once some lane is inside");
}
