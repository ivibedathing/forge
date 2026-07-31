// GPU skinning (M30): the other half of "CPU skeleton, GPU skin".
//
// The joint palette is computed on the CPU — `engine_core::skeleton::palette`,
// which is why `engine list-joints --time` can report where every joint went
// and a script can put a torch in a hand. What arrives here is ~6 KiB of
// matrices per skinned draw; the vertex data was uploaded once and never
// changes, which is the whole point (posing vertices on the CPU would mint a
// new `Arc<MeshData>` every frame and defeat M15's upload cache).
//
// Spliced ahead of `mesh.wgsl`'s object uniform by `skin_producer`, and ahead
// of the two shadow casters by their own splices — a walking character whose
// shadow does not walk is a missing pipeline that reads as a renderer bug.

const MAX_JOINTS: u32 = 128u;

// One joint's palette entry — skin space → posed skin space — as the three
// rows of an affine matrix.
//
// Three rows and not a `mat4x4`: a joint matrix's fourth column is always
// (0, 0, 0, 1), and storing it would waste a quarter of the budget. 128 joints
// cost 6 KiB packed this way against the 16 KiB
// `max_uniform_buffer_binding_size` that `downlevel_defaults` guarantees; as
// `mat4x4` the same rig costs 8 KiB and 128 becomes the ceiling rather than a
// comfortable limit.
struct JointRows {
    row0: vec4<f32>,
    row1: vec4<f32>,
    row2: vec4<f32>,
};

struct JointPalette {
    joints: array<JointRows, MAX_JOINTS>,
};

// Group 0 beside the object uniform, with its own dynamic offset — the
// arrangement `water_objects`, `cloud_objects` and `road_objects` already use.
// `downlevel_defaults` caps `max_bind_groups` at 4 and M26 spent the fourth on
// materials, so the palette has nowhere to go as a group of its own; riding in
// group 0 costs the plain pipelines nothing, because they keep the group-0
// layout they have.
@group(0) @binding(1) var<uniform> palette: JointPalette;

struct Skinned {
    position: vec3<f32>,
    normal: vec3<f32>,
};

// Linear blend skinning: the influences' matrices are averaged by weight and
// the vertex is transformed once, rather than transformed four times and the
// results averaged. The two agree for affine matrices and this is one matrix
// build instead of four transforms.
fn skin_vertex(
    position: vec3<f32>,
    normal: vec3<f32>,
    indices: vec4<u32>,
    weights: vec4<f32>,
) -> Skinned {
    var out: Skinned;

    let total = weights.x + weights.y + weights.z + weights.w;
    if total <= 0.0 {
        // A vertex with no influences at all — a static primitive sharing a
        // file with a skinned one — is left exactly where the file put it.
        // Attaching it to joint 0 instead would drag it around with a bone.
        out.position = position;
        out.normal = normal;
        return out;
    }

    var row0 = vec4<f32>(0.0);
    var row1 = vec4<f32>(0.0);
    var row2 = vec4<f32>(0.0);
    for (var i = 0u; i < 4u; i = i + 1u) {
        let weight = weights[i] / total;
        if weight == 0.0 {
            continue;
        }
        // Clamped rather than trusted: an out-of-range index is a malformed
        // export, and reading past a fixed-size uniform array is undefined
        // where a collapsed vertex is merely visible.
        let joint = min(indices[i], MAX_JOINTS - 1u);
        row0 = row0 + palette.joints[joint].row0 * weight;
        row1 = row1 + palette.joints[joint].row1 * weight;
        row2 = row2 + palette.joints[joint].row2 * weight;
    }

    let homogeneous = vec4<f32>(position, 1.0);
    out.position = vec3<f32>(
        dot(row0, homogeneous),
        dot(row1, homogeneous),
        dot(row2, homogeneous),
    );
    // The rotation part only, which is the standard approximation: a rig with
    // non-uniform joint scale would want the inverse transpose of the blend,
    // and computing one per vertex costs more than every rig in this engine
    // is worth. The entity's own `normal_matrix` still applies afterwards.
    out.normal = vec3<f32>(
        dot(row0.xyz, normal),
        dot(row1.xyz, normal),
        dot(row2.xyz, normal),
    );
    return out;
}
