//! Program linking: interface matching (ANGLE's messages), attribute
//! locations, the uniform table and uniform block layouts (std140).

use std::collections::HashMap;
use std::rc::Rc;

use crate::glsl::ast::Interp;
use crate::glsl::types::{Base, Prec, Scalar, StructDef, Type};
use crate::ir::{IoVar, Reg, Shader};

/// One active uniform (a leaf: basic type, possibly an array).
#[derive(Clone, Debug)]
pub struct Uniform {
    /// Name as `getActiveUniform` reports it (`a[0]` for arrays).
    pub name: String,
    /// Element type (no array).
    pub ty: Type,
    /// Array length (1 for non-arrays).
    pub size: u32,
    /// Offset in the program's uniform storage (cells).
    pub offset: u32,
    pub vs: Option<Reg>,
    pub fs: Option<Reg>,
    /// Uniform block member: (block, byte offset, array stride, matrix
    /// stride, row major).
    pub block: Option<(u32, u32, u32, u32, bool)>,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub name: String,
    pub data_size: u32,
    pub binding: u32,
    pub vs: bool,
    pub fs: bool,
    /// Indices into `uniforms`.
    pub members: Vec<u32>,
}

#[derive(Clone, Debug)]
pub struct Attrib {
    pub name: String,
    pub ty: Type,
    pub location: u32,
    pub cell: Reg,
}

/// A varying: VS cells → FS cells.
#[derive(Clone, Debug)]
pub struct Varying {
    pub vs: Reg,
    pub fs: Reg,
    pub cells: u32,
    pub interp: Interp,
}

#[derive(Clone, Debug)]
pub struct Program {
    pub vs: Rc<Shader>,
    pub fs: Rc<Shader>,
    pub attribs: Vec<Attrib>,
    pub uniforms: Vec<Uniform>,
    pub uniform_cells: u32,
    pub blocks: Vec<Block>,
    pub varyings: Vec<Varying>,
    /// Fragment outputs: (location, cell, components, scalar kind).
    pub outputs: Vec<(u32, Reg, u32, Scalar)>,
}

fn same_type(a: &Type, sa: &[StructDef], b: &Type, sb: &[StructDef]) -> bool {
    if a.rows != b.rows || a.cols != b.cols || a.array != b.array {
        return false;
    }
    match (a.base, b.base) {
        (Base::Struct(x), Base::Struct(y)) => {
            let (dx, dy) = (&sa[x as usize], &sb[y as usize]);
            dx.name == dy.name
                && dx.fields.len() == dy.fields.len()
                && dx
                    .fields
                    .iter()
                    .zip(&dy.fields)
                    .all(|(f, g)| f.0 == g.0 && same_type(&f.1, sa, &g.1, sb))
        }
        (x, y) => x == y,
    }
}

/// Leaves of a uniform: (name, element type, array size, cell offset
/// within the variable).
fn leaves(
    name: &str,
    ty: &Type,
    structs: &[StructDef],
    base: u32,
    out: &mut Vec<(String, Type, u32, u32, Vec<(Type, u32)>)>,
    path: Vec<(Type, u32)>,
) {
    match ty.base {
        Base::Struct(s) => {
            let def = &structs[s as usize];
            let one: u32 = Type { array: 0, ..*ty }.cells(structs);
            let n = ty.array.max(1);
            for i in 0..n {
                let prefix = if ty.array > 0 {
                    format!("{name}[{i}]")
                } else {
                    name.to_owned()
                };
                let mut off = base + i * one;
                for (fname, ft, _) in &def.fields {
                    let mut p = path.clone();
                    p.push((*ty, i));
                    leaves(&format!("{prefix}.{fname}"), ft, structs, off, out, p);
                    off += ft.cells(structs);
                }
            }
        }
        _ => {
            let reported = if ty.array > 0 {
                format!("{name}[0]")
            } else {
                name.to_owned()
            };
            out.push((reported, ty.elem(), ty.array.max(1), base, path));
        }
    }
}

/// std140 alignment and size of a type.
fn std140(ty: &Type, structs: &[StructDef], row_major: bool) -> (u32, u32) {
    let elem = |t: &Type| -> (u32, u32) {
        match t.base {
            Base::Struct(s) => {
                let mut off = 0u32;
                let mut align = 16u32;
                for (_, ft, _) in &structs[s as usize].fields {
                    let (a, sz) = std140(ft, structs, row_major);
                    align = align.max(a);
                    off = off.div_ceil(a) * a + sz;
                }
                (align, off.div_ceil(16) * 16)
            }
            _ if t.cols > 1 => {
                let (n, vec) = if row_major {
                    (t.rows, t.cols)
                } else {
                    (t.cols, t.rows)
                };
                let _ = vec;
                (16, 16 * u32::from(n))
            }
            _ => {
                let n = u32::from(t.rows);
                let a = if n == 1 {
                    4
                } else if n == 2 {
                    8
                } else {
                    16
                };
                (a, 4 * n)
            }
        }
    };
    if ty.array > 0 {
        let (a, s) = elem(&ty.elem());
        let stride = s.div_ceil(16) * 16;
        (a.max(16), stride * ty.array)
    } else {
        elem(ty)
    }
}

pub struct LinkOptions {
    pub max_attribs: u32,
    pub bound_locations: HashMap<String, u32>,
}

/// Links a program; `Err` holds the info log.
pub fn link(vs: Rc<Shader>, fs: Rc<Shader>, o: &LinkOptions) -> Result<Program, String> {
    let mut log = String::new();
    if vs.version != fs.version {
        return Err("Fragment shader version does not match other shader versions.\n".into());
    }
    // Varyings.
    let mut varyings = Vec::new();
    for fi in &fs.inputs {
        let vo = vs.outputs.iter().find(|v| v.name == fi.name);
        match vo {
            Some(v) => {
                if !same_type(&v.ty, &vs.structs, &fi.ty, &fs.structs) {
                    log.push_str(&format!(
                        "Types of varying '{}' differ between VERTEX and FRAGMENT shaders.\n",
                        fi.name
                    ));
                    log.push_str(&format!(
                        "FRAGMENT varying {} does not match any VERTEX varying\n",
                        fi.name
                    ));
                    continue;
                }
                varyings.push(Varying {
                    vs: v.cell,
                    fs: fi.cell,
                    cells: fi.ty.cells(&fs.structs),
                    interp: if vs.version >= 300 {
                        v.interp
                    } else {
                        Interp::Smooth
                    },
                });
            }
            None if fi.used => {
                log.push_str(&format!(
                    "FRAGMENT varying {} does not match any VERTEX varying\n",
                    fi.name
                ));
            }
            None => {}
        }
    }
    // Uniforms: VS declarations, then FS-only ones; samplers last.
    let mut uniforms: Vec<Uniform> = Vec::new();
    let mut cells = 0;
    let mut decls: Vec<(&IoVar, Option<&IoVar>, Option<&IoVar>)> = Vec::new();
    for u in &vs.uniforms {
        let f = fs.uniforms.iter().find(|x| x.name == u.name);
        if let Some(f) = f {
            if !same_type(&u.ty, &vs.structs, &f.ty, &fs.structs) {
                log.push_str(&format!(
                    "Types of uniform '{}' differ between VERTEX and FRAGMENT shaders.\n",
                    u.name
                ));
                continue;
            }
            if u.prec != f.prec && u.prec != Prec::None && f.prec != Prec::None && !u.ty.is_struct()
            {
                log.push_str(&format!(
                    "Precisions of uniform '{}' differ between VERTEX and FRAGMENT shaders.\n",
                    u.name
                ));
                continue;
            }
        }
        decls.push((u, Some(u), f));
    }
    for f in &fs.uniforms {
        if !vs.uniforms.iter().any(|x| x.name == f.name) {
            decls.push((f, None, Some(f)));
        }
    }
    let mut samplers = Vec::new();
    for (decl, v, f) in decls {
        let active = v.is_some_and(|x| x.used) || f.is_some_and(|x| x.used);
        if !active {
            continue;
        }
        let structs = if v.is_some() {
            &vs.structs
        } else {
            &fs.structs
        };
        let mut ls = Vec::new();
        leaves(&decl.name, &decl.ty, structs, 0, &mut ls, Vec::new());
        for (name, ty, size, off, _) in ls {
            let n = ty.cells(structs) * size;
            let u = Uniform {
                name,
                ty,
                size,
                offset: 0,
                vs: v.map(|x| x.cell + off),
                fs: f.map(|x| x.cell + off),
                block: None,
            };
            if ty.is_sampler() {
                samplers.push((u, n));
            } else {
                uniforms.push(Uniform { offset: cells, ..u });
                cells += n;
            }
        }
    }
    for (u, n) in samplers {
        uniforms.push(Uniform { offset: cells, ..u });
        cells += n;
    }
    // Uniform blocks.
    let mut blocks: Vec<Block> = Vec::new();
    for (sh, is_vs) in [(&vs, true), (&fs, false)] {
        for b in &sh.blocks {
            if !b.used {
                continue;
            }
            let idx = match blocks.iter().position(|x| x.name == b.name) {
                Some(i) => {
                    if is_vs {
                        blocks[i].vs = true;
                    } else {
                        blocks[i].fs = true;
                    }
                    // Members already listed; record this shader's cells.
                    let Base::Struct(s) = b.ty.base else { continue };
                    let def = &sh.structs[s as usize];
                    let mut off = 0;
                    for (k, (fname, ft, _)) in def.fields.iter().enumerate() {
                        let mut ls = Vec::new();
                        let full = if b.instance.is_empty() {
                            fname.clone()
                        } else {
                            format!("{}.{fname}", b.name)
                        };
                        leaves(&full, ft, &sh.structs, b.cell + off, &mut ls, Vec::new());
                        for (name, ..) in &ls {
                            if let Some(u) = uniforms.iter_mut().find(|u| {
                                &u.name == name && u.block.is_some_and(|x| x.0 == i as u32)
                            }) {
                                let cell = ls.iter().find(|l| &l.0 == name).map(|l| l.3);
                                if is_vs { u.vs = cell } else { u.fs = cell }
                            }
                        }
                        let _ = k;
                        off += ft.cells(&sh.structs);
                    }
                    continue;
                }
                None => {
                    blocks.push(Block {
                        name: b.name.clone(),
                        data_size: 0,
                        binding: b.binding.unwrap_or(0),
                        vs: is_vs,
                        fs: !is_vs,
                        members: Vec::new(),
                    });
                    blocks.len() - 1
                }
            };
            let Base::Struct(s) = b.ty.base else { continue };
            let def = &sh.structs[s as usize];
            let mut byte = 0u32;
            let mut cell_off = 0u32;
            for (k, (fname, ft, _)) in def.fields.iter().enumerate() {
                let rm = b.row_major.get(k).copied().unwrap_or(false);
                let (align, size) = std140(ft, &sh.structs, rm);
                byte = byte.div_ceil(align) * align;
                let full = if b.instance.is_empty() {
                    fname.clone()
                } else {
                    format!("{}.{fname}", b.name)
                };
                let mut ls = Vec::new();
                leaves(&full, ft, &sh.structs, cell_off, &mut ls, Vec::new());
                // Byte offsets of leaves (structs inside blocks: walk).
                let mut leaf_byte = byte;
                for (name, ty, size_n, cell, _) in ls {
                    let (la, lsz) = std140(
                        &Type {
                            array: if size_n > 1 { size_n } else { 0 },
                            ..ty
                        },
                        &sh.structs,
                        rm,
                    );
                    leaf_byte = leaf_byte.div_ceil(la) * la;
                    let stride = if size_n > 1 || ft.is_array() {
                        lsz / size_n.max(1)
                    } else {
                        0
                    };
                    let mstride = if ty.cols > 1 { 16 } else { 0 };
                    let ui = uniforms.len() as u32;
                    uniforms.push(Uniform {
                        name,
                        ty,
                        size: size_n,
                        offset: 0,
                        vs: if is_vs { Some(b.cell + cell) } else { None },
                        fs: if is_vs { None } else { Some(b.cell + cell) },
                        block: Some((idx as u32, leaf_byte, stride, mstride, rm && ty.cols > 1)),
                    });
                    blocks[idx].members.push(ui);
                    leaf_byte += lsz;
                }
                byte += size;
                cell_off += ft.cells(&sh.structs);
            }
            blocks[idx].data_size = byte.div_ceil(16) * 16;
        }
    }
    // Attributes.
    let mut attribs = Vec::new();
    let mut used_locs = vec![false; o.max_attribs as usize];
    let active: Vec<&IoVar> = vs.inputs.iter().filter(|a| a.used).collect();
    for a in &active {
        let slots = u32::from(a.ty.cols.max(1));
        let loc = a
            .location
            .or_else(|| o.bound_locations.get(&a.name).copied());
        if let Some(l) = loc {
            for k in 0..slots {
                if let Some(s) = used_locs.get_mut((l + k) as usize) {
                    *s = true;
                }
            }
            attribs.push(Attrib {
                name: a.name.clone(),
                ty: a.ty,
                location: l,
                cell: a.cell,
            });
        }
    }
    for a in &active {
        if attribs.iter().any(|x| x.name == a.name) {
            continue;
        }
        let slots = a.ty.cols.max(1) as usize;
        let free = (0..used_locs.len())
            .find(|&i| i + slots <= used_locs.len() && used_locs[i..i + slots].iter().all(|u| !u));
        match free {
            Some(l) => {
                for s in &mut used_locs[l..l + slots] {
                    *s = true;
                }
                attribs.push(Attrib {
                    name: a.name.clone(),
                    ty: a.ty,
                    location: l as u32,
                    cell: a.cell,
                });
            }
            None => {
                log.push_str(&format!("Too many attributes ({})\n", a.name));
            }
        }
    }
    attribs.sort_by_key(|a| active.iter().position(|x| x.name == a.name));
    // Fragment outputs.
    let mut outputs = Vec::new();
    if fs.version >= 300 {
        for (i, out) in fs.outputs.iter().enumerate() {
            let loc = out.location.unwrap_or(i as u32);
            let k = out.ty.scalar_kind().unwrap_or(Scalar::Float);
            for e in 0..out.ty.array.max(1) {
                outputs.push((
                    loc + e,
                    out.cell + e * u32::from(out.ty.rows),
                    u32::from(out.ty.rows),
                    k,
                ));
            }
        }
    } else if fs.frag_data > 0 {
        let base = fs.builtins[&crate::glsl::ast::Builtin::FragData];
        for e in 0..fs.frag_data {
            outputs.push((e, base + e * 4, 4, Scalar::Float));
        }
    } else if let Some(c) = fs.builtins.get(&crate::glsl::ast::Builtin::FragColor) {
        outputs.push((0, *c, 4, Scalar::Float));
    }
    if !log.is_empty() {
        return Err(log);
    }
    Ok(Program {
        vs,
        fs,
        attribs,
        uniforms,
        uniform_cells: cells,
        blocks,
        varyings,
        outputs,
    })
}

impl Program {
    /// Uniform index and array element for a `getUniformLocation` name.
    pub fn location(&self, name: &str) -> Option<(u32, u32)> {
        for (i, u) in self.uniforms.iter().enumerate() {
            if u.block.is_some() {
                continue;
            }
            let base = u.name.strip_suffix("[0]").unwrap_or(&u.name);
            if name == u.name || name == base {
                return Some((i as u32, 0));
            }
            if u.size > 1
                && let Some(rest) = name.strip_prefix(base)
                && let Some(idx) = rest.strip_prefix('[').and_then(|r| r.strip_suffix(']'))
                && let Ok(k) = idx.parse::<u32>()
                && k < u.size
            {
                return Some((i as u32, k));
            }
        }
        None
    }
}
