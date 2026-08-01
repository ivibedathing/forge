// Arena Shooter — the whole game, in one script.
//
//   WASD    move          (world-relative, the way a top-down game moves)
//   Mouse   aim           (the gun points at the ground under the cursor)
//   Click   fire
//   1 2 3   pistol / rifle / shotgun
//   R       reload
//   Esc     pause
//
// The run starts on a main menu and pauses to one. Both are a `HudPanel` tree
// in the scene file (M31): the engine lays them out and hit-tests them, and
// what is left here is which words are on screen and what a click means. A menu
// is still game logic — `world.clicked` is polled, nothing dispatches into a
// script — but the *layout* stopped being game logic in M31.
//
// Three waves of hovering drones home in on the player. Shoot them, or shoot a
// barrel and let the blast take everything near it. Health runs out, the run
// ends; clear all ten and the next level flies in.
//
// How the pieces fit, since none of it is obvious from the scene file:
//
// * `Player` is a physics proxy with no mesh. A dynamic body owns its own
//   Transform, so a script cannot turn one to face the aim — `PlayerModel` is
//   a separate visual entity placed from the proxy's position every step. That
//   is M12's wheel pattern, and M36 put a rigged character on the end of it
//   where a cylinder and a sphere used to stand in for a person.
// * The weapon is hung off the rig's `HandR` with `world.joint_position`,
//   which is M30's sanctioned way to hold a prop. Three exist; two are parked
//   below the floor, because a `Mesh` has no visibility flag and parking is
//   what the drones, the barrels and the bullet pool all already do.
// * Bullets are not physics. Twenty-four mesh-only entities are recycled here,
//   flown at BULLET_SPEED, and tested against drone centres as a swept
//   *segment* — a 46 m/s round covers 0.77 m per step, so a point test would
//   shoot straight through a drone between two frames. Nothing about them
//   touches rapier, which is also why a hail of fire cannot disturb the arena.
//   Known limit: they pass over cover. Cover stops bodies, not bolts.
// * All run state is `world.state` — numeric, per-run, replay-deterministic.
//   Since M36 it is also what a *save* is: `world.save(slot)` writes this map
//   to a file, so score, level, health and every setting persist without any
//   of them being enumerated anywhere. Entity names are built from indices,
//   which is why the counts below must match `make_arena.py`.
// * Every `Breakable` here is script-only (no `impulse_threshold`), so physics
//   can never open one behind this script's back and leave an alive flag lying
//   about an entity that no longer exists.
//
// WHY THIS IS WRITTEN SO FLAT, which is the thing most likely to be "tidied"
// back into a bug: Rhai's *function* expression-depth limit is **16 in a debug
// build** (32 in release), and blocks are expensive against it. Measured on
// this engine: three nested blocks inside `fn step` is the ceiling, and a
// `let x = world.something(...)` read only survives two. `bin/engine` is a
// debug binary, so a script that nests one loop deeper than this compiles
// under a release build and fails to *parse* under the one the repo uses.
// That is why every inner loop below lives in its own function — a function
// body starts the budget over — and why subexpressions are pulled out into
// locals rather than nested.
//
// There is no randomness anywhere in here, so a recorded input timeline
// replays to the same pixels. The shotgun's spread and the rifle's cone are
// *fixed patterns* for exactly that reason — see `spray` below.

fn pad2(n) {
    if n < 10 {
        return "0" + n.to_string();
    }
    n.to_string()
}

fn drone_name(n) {
    "Drone" + pad2(n)
}

fn barrel_name(n) {
    "Barrel" + n.to_string()
}

fn clampf(v, lo, hi) {
    if v < lo { return lo; }
    if v > hi { return hi; }
    v
}

// Squared distance from point c to the segment p->q. This is the hit test: a
// bullet's travel over one step, against a drone's centre. Squared, because
// every caller compares it to a constant radius and a square root would buy
// nothing.
fn seg_dist2(px, py, pz, qx, qy, qz, cx, cy, cz) {
    let dx = qx - px;
    let dy = qy - py;
    let dz = qz - pz;
    let len2 = dx * dx + dy * dy + dz * dz;
    let t = 0.0;
    if len2 > 0.000001 {
        let wx = cx - px;
        let wy = cy - py;
        let wz = cz - pz;
        let dot = wx * dx + wy * dy + wz * dz;
        t = dot / len2;
        t = clampf(t, 0.0, 1.0);
    }
    let ex = px + dx * t - cx;
    let ey = py + dy * t - cy;
    let ez = pz + dz * t - cz;
    ex * ex + ey * ey + ez * ez
}

fn dist2(ax, ay, az, bx, by, bz) {
    let dx = ax - bx;
    let dy = ay - by;
    let dz = az - bz;
    dx * dx + dy * dy + dz * dz
}

// --- the weapons (M36) ------------------------------------------------------
//
// Three of them, and each number is a lookup rather than a table because Rhai
// arrays of arrays cost depth this script does not have. Index 0 is the
// sidearm the run starts with; the order matches `WEAPONS` in `make_arena.py`,
// and 1/2/3 select them.
//
// The pistol is the gun that shipped, with one number moved: 0.13 s between
// shots became 0.20, so the rifle has somewhere to be faster. Everything else
// about it — 34 damage, twelve rounds, three bolts to a level-1 kill — is
// unchanged, which keeps the opening fight the one that has always been here.

fn wep_entity(w) {
    if w == 0 { return "WeaponPistol"; }
    if w == 1 { return "WeaponRifle"; }
    "WeaponShotgun"
}

fn wep_label(w) {
    if w == 0 { return "PISTOL"; }
    if w == 1 { return "RIFLE"; }
    "SHOTGUN"
}

fn wep_interval(w) {
    if w == 0 { return 0.20; }
    if w == 1 { return 0.09; }
    0.62
}

fn wep_damage(w) {
    if w == 0 { return 34.0; }
    if w == 1 { return 24.0; }
    21.0
}

fn wep_mag(w) {
    if w == 0 { return 12; }
    if w == 1 { return 30; }
    6
}

fn wep_reload(w) {
    if w == 0 { return 1.1; }
    if w == 1 { return 1.6; }
    1.9
}

// Bolts per trigger pull. The shotgun's five are what makes it a shotgun; the
// pool is sized (24) so a full spread plus a rifle burst still fits.
fn wep_pellets(w) {
    if w == 2 { return 5; }
    1
}

// Half-angle of the pattern, in degrees.
fn wep_spread(w) {
    if w == 0 { return 0.0; }
    if w == 1 { return 1.5; }
    10.0
}

// How far ahead of the grip each weapon's muzzle is — the numbers
// `make_weapons.py` prints when it writes the meshes. The flash, the smoke and
// the bolt all start here, so a shotgun's blast leaves its own barrel rather
// than the pistol's.
fn wep_muzzle(w) {
    if w == 0 { return 0.24; }
    if w == 1 { return 0.62; }
    0.52
}

// Rotate an aim vector in the XZ plane. Two functions rather than one because
// a Rhai function returns one value and an array here would allocate per
// pellet; used only for spread, which is why it takes and returns the pair.
fn turn_x(ax, az, degrees) {
    let r = degrees * 0.017453292;
    ax * r.cos() - az * r.sin()
}

fn turn_z(ax, az, degrees) {
    let r = degrees * 0.017453292;
    ax * r.sin() + az * r.cos()
}

// The first free bullet slot, or -1. Scanning beats a round-robin index: it
// never overwrites a bolt still in flight.
fn free_slot(world, count) {
    let slot = -1;
    for i in 0..count {
        if slot >= 0 { continue; }
        let life = world.state("b" + i.to_string() + "t", 0.0);
        if life <= 0.0 { slot = i; }
    }
    slot
}

// Put `pellets` bolts in the air. Returns how many actually launched — fewer
// than asked if the pool is dry, which is a real outcome rather than an error.
//
// **The pattern is fixed, never random**, and that is not a shortcut: a script
// has no randomness at all (M10, and it is load-bearing — a recorded timeline
// has to replay to the same pixels), so a random cone was never available. A
// fixed one is also better for the demo: the same click kills the same drone
// every replay. `walk` shifts a single-bolt weapon's cone by shot count, so a
// held rifle trigger climbs and settles instead of drilling one hole.
fn spray(world, pellets, pool, mux, muy, muz, ax, az, speed, life, spread, walk, damage) {
    let launched = 0;
    for p in 0..pellets {
        let slot = free_slot(world, pool);
        if slot < 0 { continue; }
        // Symmetric about the aim: -spread at p = 0, +spread at the last
        // pellet, and exactly 0 for a single bolt with no walk.
        let offset = spread * walk;
        let span = (pellets - 1).to_float();
        if pellets > 1 { offset = spread * (2.0 * p.to_float() / span - 1.0); }
        let dx = turn_x(ax, az, offset);
        let dz = turn_z(ax, az, offset);
        let key = "b" + slot.to_string();
        world.set_position("Bullet" + pad2(slot), mux, muy, muz);
        world.set_state(key + "t", life);
        world.set_state(key + "x", dx * speed);
        world.set_state(key + "z", dz * speed);
        // Damage rides on the bolt, not on the weapon: switching guns must not
        // retroactively re-arm rounds already in flight.
        world.set_state(key + "d", damage);
        launched += 1;
    }
    launched
}

// Which wave a drone belongs to, from its index *within its level* (1..10).
// Waves are contiguous index ranges; the generator prints these boundaries
// when it writes the scene.
fn drone_wave(local) {
    if local <= 4 { return 1; }
    if local <= 7 { return 2; }
    3
}

// The first drone and the first barrel of a level, and how many barrels it has.
// Every level owns a contiguous run of each, which is what lets one arithmetic
// expression get from "level 3" to "Drone21..Drone30" — the generator prints
// exactly these boundaries.
fn drone_base(level, per_level) {
    (level - 1) * per_level
}

fn barrels_in(level) {
    if level == 1 { return 4; }
    if level == 2 { return 3; }
    if level == 3 { return 2; }
    1
}

fn barrel_first(level) {
    if level == 1 { return 1; }
    if level == 2 { return 5; }
    if level == 3 { return 8; }
    10
}

// The first live drone the segment b->n reaches, as a 0-based index, or -1.
// The position arrays are passed by value and only read.
fn hit_drone(alive, dxs, dys, dzs, bx, by, bz, nx, ny, nz, r2) {
    let found = -1;
    let count = alive.len();
    for j in 0..count {
        if found >= 0 { continue; }
        if !alive[j] { continue; }
        let cx = dxs[j];
        let cy = dys[j];
        let cz = dzs[j];
        let d2 = seg_dist2(bx, by, bz, nx, ny, nz, cx, cy, cz);
        if d2 <= r2 { found = j; }
    }
    found
}

// The first unbroken barrel the segment reaches, 1-based, or -1.
fn hit_barrel(world, first, count, bx, by, bz, nx, ny, nz, r2) {
    let found = -1;
    for j in 0..count {
        if found >= 0 { continue; }
        let n = first + j;
        let flag = world.state("k" + n.to_string() + "a", 1);
        if flag <= 0.5 { continue; }
        let name = barrel_name(n);
        let bar = world.position(name);
        let ox = bar[0];
        let oy = bar[1];
        let oz = bar[2];
        let d2 = seg_dist2(bx, by, bz, nx, ny, nz, ox, oy, oz);
        if d2 <= r2 { found = n; }
    }
    found
}

// Take `dmg` off drone `n`. Returns 1 if that killed it — the break and the
// alive flag are set here so the caller never has to do either at depth.
fn damage_drone(world, n, dmg, full_hp) {
    let key = "d" + n.to_string() + "h";
    let hp = world.state(key, full_hp) - dmg;
    world.set_state(key, hp);
    if hp > 0.0 { return 0; }
    let name = drone_name(n);
    world.break_entity(name);
    world.set_state("d" + n.to_string() + "a", 0);
    1
}

// Blow up barrel `n`: break it, kick the world, kill every live drone inside
// the radius, and chain into any other barrel in range (one link deep —
// deeper would want a worklist, and one link is enough for a cluster to go up
// together). Returns the 0-based indices of the drones it killed, so the
// caller can update the arrays it is iterating.
fn detonate(world, n, first, barrel_count, base, alive, dxs, dys, dzs, r2, impulse) {
    let radius = r2.sqrt();
    let name = barrel_name(n);
    let bar = world.position(name);
    let ox = bar[0];
    let oy = bar[1];
    let oz = bar[2];
    world.set_state("k" + n.to_string() + "a", 0);
    world.break_entity(name);
    world.explode(ox, oy, oz, radius, impulse);

    let killed = [];
    let drones = alive.len();
    for k in 0..drones {
        if !alive[k] { continue; }
        let ex = dxs[k];
        let ey = dys[k];
        let ez = dzs[k];
        let d2 = dist2(ex, ey, ez, ox, oy, oz);
        if d2 > r2 { continue; }
        let m = base + k + 1;
        let dname = drone_name(m);
        world.break_entity(dname);
        world.set_state("d" + m.to_string() + "a", 0);
        killed.push(k);
    }

    for k in 0..barrel_count {
        let m = first + k;
        if m == n { continue; }
        let flag = world.state("k" + m.to_string() + "a", 1);
        if flag <= 0.5 { continue; }
        let other = world.position(barrel_name(m));
        let qx = other[0];
        let qy = other[1];
        let qz = other[2];
        let d2 = dist2(qx, qy, qz, ox, oy, oz);
        if d2 > r2 { continue; }
        world.set_state("k" + m.to_string() + "a", 0);
        world.break_entity(barrel_name(m));
        world.explode(qx, qy, qz, radius, impulse);
    }
    killed
}

// Bring every barrel that is in play down to rest height and keep it there.
//
// A barrel has `gravity_scale: 0` — it has to, or the ones waiting three levels
// out would sag out of the sky — so nothing makes it fall on its own and
// nothing makes it stay put after a blast shoves it. Both are this loop: drive
// it down until it is at rest height, then hold it at zero velocity. A script
// cannot move a dynamic body by writing its Transform (M11.5), so a drop is a
// velocity or it is nothing.
fn settle_barrels(world, first, count, rest) {
    for j in 0..count {
        let n = first + j;
        let flag = world.state("k" + n.to_string() + "a", 1);
        if flag <= 0.5 { continue; }
        let name = barrel_name(n);
        let at = world.position(name);
        let gap = at[1] - rest;
        let fall = 0.0;
        // Eased so it lands rather than slamming: fast while it is high, down
        // to a crawl over the last metre.
        if gap > 0.03 { fall = clampf(gap * 4.0, 2.0, 22.0); }
        world.set_linear_velocity(name, 0.0, -fall, 0.0);
    }
}

// Fly one drone at the player, or hold it still if its wave has not started.
// Returns 1 if it is close enough to be hurting the player.
fn drive_drone(world, n, ex, ey, ez, px, py, pz, speed, hover, touch) {
    let name = drone_name(n);
    let ddx = px - ex;
    let ddz = pz - ez;
    let flat2 = ddx * ddx + ddz * ddz;
    let flat = flat2.sqrt();
    let sx = 0.0;
    let sz = 0.0;
    if flat > 0.0001 {
        sx = ddx / flat;
        sz = ddz / flat;
    }
    // Close in, then back out of contact range rather than grinding through
    // the player — a drone that gives ground reads as attacking, not stuck.
    let drive = speed;
    let hold = touch * 0.85;
    if flat < hold { drive = -speed * 0.4; }
    let rise = (hover - ey) * 3.0;
    let dvy = clampf(rise, -14.0, 6.0);
    let dvx = sx * drive;
    let dvz = sz * drive;
    world.set_linear_velocity(name, dvx, dvy, dvz);

    if flat > touch { return 0; }
    let gap = ey - py;
    if gap.abs() > 1.6 { return 0; }
    1
}

// --- the menu (M36) ---------------------------------------------------------
//
// Seven button slots in the scene, labelled per screen. An empty label hides a
// slot, and a hidden element leaves the flow entirely (M31), so each card is
// exactly as tall as the screen it is showing — the same property that already
// made the end card close up around its missing button. Nothing in the scene
// knows what a screen is.

fn set_slot(world, n, label) {
    let name = "MenuBtn" + n.to_string();
    world.set_hud_visible(name, label != "");
    world.set_hud_text(name + "Text", label);
}

// Paint a whole card: the words at the top, then a label per slot, then blanks
// for the rest.
fn paint(world, title, line, labels, slots) {
    world.set_hud_visible("MenuRoot", true);
    world.set_hud_text("MenuTitle", title);
    world.set_hud_text("MenuLine", line);
    let n = labels.len();
    for i in 0..slots {
        let label = "";
        if i < n { label = labels[i]; }
        set_slot(world, i + 1, label);
    }
}

// Which slot was clicked this step, 1-based, or -1. `world.clicked` is the
// engine's answer, not a rectangle test: M31 hit-tests the element's laid-out
// box before scripts run and it is true for exactly one step. A hidden slot is
// not hit-tested, so a screen cannot be clicked through to the one behind it.
fn clicked_slot(world, slots) {
    let found = -1;
    for i in 0..slots {
        let name = "MenuBtn" + (i + 1).to_string();
        if world.clicked(name) { found = i + 1; }
    }
    found
}

// A three-way setting's label. Settings are ordinary `world.state` keys, which
// is what makes them saveable for free: `world.save` writes the whole map, so
// it carries them without knowing they exist.
fn opt3(v, a, b, c) {
    if v == 0 { return a; }
    if v == 1 { return b; }
    c
}

fn opt3f(v, a, b, c) {
    if v == 0 { return a; }
    if v == 1 { return b; }
    c
}

fn row(name, value) {
    name + "   " + value
}

// The health gauge's colour, green through amber to red. Returns [r, g, b] —
// the fill is one rect and `set_hud_color` is one call, so a bar that says how
// bad it is costs three lines rather than three components.
fn health_color(fraction) {
    let hurt = clampf((1.0 - fraction) * 2.0, 0.0, 1.0);
    let dying = clampf((0.5 - fraction) * 2.5, 0.0, 1.0);
    let r = 0.25 + hurt * 0.65;
    let g = 0.85 - dying * 0.72;
    let b = 0.42 - hurt * 0.30;
    [r, g, b]
}

// Park the two weapons the player is not holding below the floor, where a
// spent bullet already goes.
fn stow_weapons(world, active) {
    for w in 0..3 {
        if w == active { continue; }
        world.set_position(wep_entity(w), 0.0, -30.0, 0.0);
    }
}

// Push the four floodlights to one intensity. The arena has exactly four and
// the names are the addresses (invariant 4).
fn set_lamps(world, intensity) {
    for i in 1..5 {
        world.set_light_intensity("Lamp" + i.to_string(), intensity);
    }
}

fn step(world, step) {
    let dt = world.dt();

    // --- constants, matching make_arena.py ---------------------------------
    let LEVELS = 4;              // the campaign; the file carries every level's
    let DRONE_COUNT = 10;        // drones *per level* — 40 exist, 10 are in play
    let BULLET_COUNT = 24;
    let ARENA_HALF = 28.0;
    let WAVES = 3;
    let SLOTS = 7;               // menu button slots in the scene
    let BARREL_REST = 0.62;      // m, where a dropped barrel comes to rest
    let SAVE_SLOT = 0;           // the one slot this game uses, of the ten

    let MOVE_SPEED = 7.2;        // m/s
    let MOVE_ACCEL = 16.0;       // 1/s, how fast velocity reaches the target
    let AIM_DEADZONE = 0.7;      // m, cursor closer than this leaves the aim alone
    let BULLET_SPEED = 46.0;     // m/s
    let BULLET_LIFE = 1.05;      // s
    let HIT_R2 = 0.9;            // m^2, bolt-vs-drone (~0.95 m)
    let BARREL_R2 = 1.1;         // m^2, bolt-vs-barrel

    // The player rig (M36). `PLAYER_SOLE` is how far the capsule's bottom sits
    // below the proxy's origin, so the model's feet land on the floor rather
    // than in it; `PLAYER_STRIDE` is the ground one cycle of `Run` covers, as
    // `engine list-joints` measured it off the clip (M32).
    let SOLDIER = "../meshes/rigged_soldier.gltf";
    let PLAYER_SOLE = 0.72;
    let PLAYER_STRIDE = 2.7664;
    let LAMP_FULL = 22.0;        // the intensity the scene authors

    let DRONE_SPEED = 4.4;       // m/s, at level 1
    let DRONE_HOVER = 0.95;      // m, the altitude they hold
    let DRONE_HP = 100.0;        // at level 1
    let TOUCH_RANGE = 1.3;       // m, where a drone starts hurting
    let TOUCH_DPS = 26.0;        // at level 1
    let DRONE_SCORE = 100;

    // How a level gets harder. Three dials rather than one, because raising
    // only hit points makes a level longer where raising speed and damage makes
    // it tighter — and score follows difficulty, so a level-4 kill is worth
    // more than a level-1 one.
    //
    // The numbers are picked against two ceilings rather than by feel. Hull
    // steps so the bolts-to-a-kill count lands on 3, 4, 5, 6 with the pistol —
    // a fraction of a bolt either way is a magazine that runs out in a
    // different place for no reason the player can see. And closing speed
    // stays under MOVE_SPEED at level 4 (6.3 against 7.2), because a drone
    // that outruns the player cannot be kited and the arena stops being a
    // place to move through.
    let SPEED_STEP = 0.14;       // +14% closing speed per level
    let HP_STEP = 0.33;          // +33% hull per level: 3, 4, 5 and 6 bolts
    let DPS_STEP = 0.25;         // +25% contact damage per level
    let SCORE_STEP = 0.50;       // +50% score per level
    let HEAL_ON_LEVEL = 45.0;    // health handed back between levels

    let BLAST_R2 = 56.25;        // (7.5 m)^2
    let BLAST_IMPULSE = 260.0;
    let BARREL_SCORE = 50;

    // HUD numbers the scene has to agree with; `make_arena.py` prints them.
    let HEALTH_FILL = 218.0;     // px, the gauge panel less its 3 px bezel
    // The reticle is ui_icon.png at its *own* size, and that is load-bearing:
    // a HudImage with no `slice` is all middle band, and the middle band tiles.
    // A 32 px box around a 16 px icon draws four reticles, not a bigger one.
    let CROSSHAIR = 16.0;        // px
    let BANNER_TIME = 1.5;       // s a wave announcement stays up

    let CAM_HEIGHT = 20.0;
    let CAM_BACK = 11.5;
    let CAM_LEAD = 2.5;          // m the view leans toward the aim
    let CAM_FOLLOW = 7.0;        // 1/s

    // --- input, read once and unconditionally -------------------------------
    // Reading it inside a branch would nest another block for nothing (see
    // the depth note above).
    let k_w = world.key("KeyW");
    let k_s = world.key("KeyS");
    let k_a = world.key("KeyA");
    let k_d = world.key("KeyD");
    let k_reload = world.key("KeyR");
    let k_pause = world.key("Escape");
    let k_one = world.key("Digit1");
    let k_two = world.key("Digit2");
    let k_three = world.key("Digit3");
    let m_fire = world.mouse("MouseLeft");

    // The cursor in HUD pixels: the fraction the timeline carries, times the
    // frame it is being replayed into.
    let vw = world.viewport_width().to_float();
    let vh = world.viewport_height().to_float();
    // Named cur_x/cur_y rather than cx/cy on purpose: the camera section below
    // binds cx/cy to the *eye's* position, and Rhai's `let` rebinds in the
    // same scope, so a cursor called cx silently becomes a camera 300 lines
    // later. That cost a render: the crosshair drew itself at the camera's
    // world coordinates, 20 metres up the left edge of the frame.
    let cur_x = world.cursor_x() * vw;
    let cur_y = world.cursor_y() * vh;

    // --- settings (M36) -----------------------------------------------------
    // Read every step, before anything uses them. The defaults are the values
    // the scene file authors, so a run that never opens the settings screen is
    // arithmetically the game that shipped — the identity, not an
    // approximation of it.
    let s_diff = world.state("sdiff", 0).to_int();     // 0 normal 1 hard 2 brutal
    let s_cross = world.state("scross", 0).to_int();   // 0 on 1 off
    let s_hud = world.state("shud", 0).to_int();       // 0 full 1 minimal 2 off
    let s_light = world.state("slight", 2).to_int();   // 0 off 1 low 2 full
    let s_shadow = world.state("sshadow", 0).to_int(); // 0 on 1 off
    let s_qual = world.state("squal", 1).to_int();     // 0 low 1 high

    // Difficulty moves hull and contact damage hard and closing speed barely,
    // and that asymmetry is deliberate: at level 4 the drones already close at
    // 6.3 m/s against the player's 7.2, so a difficulty that scaled speed like
    // it scales hull would put them *ahead* of the player and the arena would
    // stop being a place to move through. 6% a step keeps BRUTAL level 4 at
    // 7.05 — still, just, kiteable.
    let rung_d = s_diff.to_float();
    let diff_hp = 1.0 + 0.45 * rung_d;
    let diff_dps = 1.0 + 0.35 * rung_d;
    let diff_speed = 1.0 + 0.06 * rung_d;
    let diff_score = 1.0 + 0.5 * rung_d;

    // --- mode: 0 title, 1 playing, 2 paused, 3 over, 4 cleared, 5 settings ---
    // Edges are the difference against what last step stored, which is the
    // whole reason the input API has no pressed/released query: two lines of
    // `world.state` per button, in the one script that wants them.
    let mode = world.state("mode", 0).to_int();
    let level = world.state("level", 1).to_int();
    let dead = world.state("dead", 0);
    let won = world.state("won", 0);
    let cleared = world.state("cleared", 0);
    // The settings screen outranks the run's own state: it is a screen the
    // player asked for, and a level clearing behind it must not steal it.
    let settling = mode == 5;
    if cleared > 0.5 && !settling { mode = 4; }
    if dead > 0.5 && !settling { mode = 3; }
    if won > 0.5 && !settling { mode = 3; }

    // Escape still needs its own edge, because a key is a held-state predicate
    // with no element behind it — which is the line the input API draws, and
    // why this game keeps one edge and gets the other from the engine.
    let pause_was = world.state("esc", 0) > 0.5;
    let paused_now = k_pause && !pause_was;
    world.set_state("esc", if k_pause { 1 } else { 0 });

    let hit = clicked_slot(world, SLOTS);
    let has_save = world.has_save(SAVE_SLOT);
    // Whether a run has actually started, which is *not* "mode > 0": the
    // settings screen is reachable from the title card, and a HUD reporting
    // SCORE 0 and WAVE 1/3 over it is reporting on a game nobody has begun.
    // Found by rendering the settings screen.
    let begun = world.state("begun", 0) > 0.5;

    // Every transition is guarded on the mode this step *started* in, and
    // lands in `next`. Assigning `mode` in place instead reads as the same
    // thing and is not: `mode == 1 && esc -> 2` followed by `mode == 2 && esc
    // -> 1` fires both in one step, and Escape does nothing at all. That cost
    // a render to notice, because the pause screen simply never appeared.
    let next = mode;
    let back_to = world.state("setback", 0).to_int();

    // Title card: PLAY / SETTINGS / LOAD GAME / QUIT.
    if mode == 0 && hit == 1 { next = 1; }
    if mode == 0 && hit == 2 { next = 5; }
    if mode == 0 && hit == 3 && has_save { next = 1; }
    // Pause card: RESUME / SAVE GAME / SETTINGS / QUIT.
    if mode == 1 && paused_now { next = 2; }
    if mode == 2 && paused_now { next = 1; }
    if mode == 2 && hit == 1 { next = 1; }
    if mode == 2 && hit == 3 { next = 5; }
    // Settings: BACK returns to whichever card opened it.
    if mode == 5 && hit == SLOTS { next = back_to; }
    // The level card offers the next one.
    if mode == 4 && hit == 1 { next = 1; }

    // Remember where BACK goes, on the step that opens the screen.
    if mode == 0 && hit == 2 { world.set_state("setback", 0); }
    if mode == 2 && hit == 3 { world.set_state("setback", 2); }

    // Quitting (M36). The engine's answer to a button, and the *caller*
    // decides what it means: the viewer closes its window, a headless run
    // stops stepping and says which step it stopped on.
    let quitting = mode == 0 && hit == 4;
    if mode == 2 && hit == 4 { quitting = true; }
    if quitting { world.quit(); }

    // Saving and loading. A save is the *campaign*, not the arena: the engine
    // cannot spawn an entity and a broken drone cannot be put back, so LOAD is
    // offered on the title card only, where every drone is still intact and
    // "level 3" means level 3's ten flying down from park altitude. Saving is
    // offered from the pause card, where there is a run to save.
    if mode == 2 && hit == 2 { world.save(SAVE_SLOT); }
    let loading = mode == 0 && hit == 3 && has_save;
    if loading { world.load(SAVE_SLOT); }

    // Cycling a settings row. Each is one `world.state` key, so the save
    // carries them for free.
    if mode == 5 && hit == 1 { world.set_state("sdiff", (s_diff + 1) % 3); }
    if mode == 5 && hit == 2 { world.set_state("scross", (s_cross + 1) % 2); }
    if mode == 5 && hit == 3 { world.set_state("shud", (s_hud + 1) % 3); }
    if mode == 5 && hit == 4 { world.set_state("slight", (s_light + 1) % 3); }
    if mode == 5 && hit == 5 { world.set_state("sshadow", (s_shadow + 1) % 2); }
    if mode == 5 && hit == 6 { world.set_state("squal", (s_qual + 1) % 2); }

    let advance = mode == 4 && hit == 1;
    mode = next;
    world.set_state("mode", mode);
    if mode == 1 { world.set_state("begun", 1); }
    if mode == 1 { begun = true; }

    // A load replaced the whole state map, `mode` included — and the save was
    // taken from a pause screen, so it says 2. The run has to start *playing*,
    // on the level that came back, with this level's ten drones still in the
    // sky waiting for their wave.
    if loading {
        mode = 1;
        world.set_state("mode", 1);
        world.set_state("wave", 1);
        world.set_state("cleared", 0);
        world.set_state("dead", 0);
        world.set_state("won", 0);
        level = world.state("level", 1).to_int();
    }

    // Starting the next level is bookkeeping and nothing else: its ten drones
    // and its barrels are already in the file, parked at PARK_Y, and the wave
    // machinery below flies them in exactly as it flies in a wave. Order
    // matters — the wave counter and the magazine are read further down, so
    // they are written here, before anything looks at them.
    if advance {
        level += 1;
        world.set_state("level", level);
        world.set_state("cleared", 0);
        world.set_state("wave", 1);
        world.set_state("reload", 0.0);
    }
    let base = drone_base(level, DRONE_COUNT);
    // Every barrel up to and including this level's is in play — not just the
    // ones this level brought. A barrel the player did not shoot last level is
    // still standing in the arena, and an unarmed barrel that looks exactly
    // like an armed one is the worst answer available; carrying them forward
    // also means saving one is worth something.
    let keg_live = barrel_first(level) + barrels_in(level) - 1;

    // This level's dials, times the difficulty's. Level 1 on NORMAL is exactly
    // the game that shipped: every step is a multiple of (level - 1) and of
    // the difficulty index, so the arithmetic is the identity there.
    let rung = (level - 1).to_float();
    let drone_speed = DRONE_SPEED * (1.0 + SPEED_STEP * rung) * diff_speed;
    let drone_hp = DRONE_HP * (1.0 + HP_STEP * rung) * diff_hp;
    let touch_dps = TOUCH_DPS * (1.0 + DPS_STEP * rung) * diff_dps;
    let kill_score = DRONE_SCORE.to_float() * (1.0 + SCORE_STEP * rung) * diff_score;

    let playing = mode == 1;
    // The game's clock. Everything that moves reads `gdt`, so a menu freezes
    // the arena by arithmetic rather than by wrapping the whole script in a
    // branch — which the expression-depth budget could not afford anyway.
    let gdt = dt;
    if !playing { gdt = 0.0; }

    // --- graphics settings, applied through the engine (M36) ----------------
    // `environment` was read off the file at load and never written again
    // before this milestone. Written every step rather than on change: the
    // write is idempotent, and the viewer already rebuilds its pipelines only
    // when `samples` actually moves.
    world.set_shadows(s_shadow == 0);
    let want_samples = 4;
    if s_qual == 0 { want_samples = 1; }
    world.set_samples(want_samples);
    let want_fog = 0.0012;
    if s_qual == 0 { want_fog = 0.0; }
    world.set_fog(want_fog);
    set_lamps(world, LAMP_FULL * opt3f(s_light, 0.0, 0.45, 1.0));

    // --- the player ---------------------------------------------------------
    let p = world.position("Player");
    let px = p[0];
    let py = p[1];
    let pz = p[2];
    let v = world.linear_velocity("Player");
    let vy = v[1];

    let mx = 0.0;
    let mz = 0.0;
    if k_w { mz -= 1.0; }
    if k_s { mz += 1.0; }
    if k_a { mx -= 1.0; }
    if k_d { mx += 1.0; }
    if !playing {
        mx = 0.0;
        mz = 0.0;
    }
    let mlen2 = mx * mx + mz * mz;
    if mlen2 > 0.0001 {
        let mlen = mlen2.sqrt();
        mx /= mlen;
        mz /= mlen;
    }
    let blend = clampf(gdt * MOVE_ACCEL, 0.0, 1.0);
    let want_vx = mx * MOVE_SPEED;
    let want_vz = mz * MOVE_SPEED;
    let vx = v[0] + (want_vx - v[0]) * blend;
    let vz = v[2] + (want_vz - v[2]) * blend;
    // A paused game stops dead rather than coasting: the velocity blend above
    // is a no-op at gdt = 0, so the stop has to be written.
    if !playing {
        vx = 0.0;
        vz = 0.0;
    }
    world.set_linear_velocity("Player", vx, vy, vz);

    // --- aim ----------------------------------------------------------------
    // Kept as a unit vector rather than an angle: `look_at` takes a point, so
    // nothing here needs an inverse tangent, and there is no wrap to get wrong.
    //
    // The mouse sets it *directly* — no slew. The arrow keys had one because a
    // held key is a rate; a cursor is already a position, and easing toward it
    // is lag with no gameplay behind it. What the engine gives is the point on
    // the ground under the cursor; the aim is the direction from the player to
    // that point.
    let ax = world.state("aimx", 0.0);
    let az = world.state("aimz", -1.0);
    let alen2 = ax * ax + az * az;
    if alen2 < 0.0001 {
        ax = 0.0;
        az = -1.0;
    }

    let ground = world.cursor_ground(py);
    let tx = ground[0] - px;
    let tz = ground[2] - pz;
    let tlen2 = tx * tx + tz * tz;
    // Inside AIM_DEADZONE the direction is noise — the cursor is on top of the
    // player — so the last aim stands rather than spinning the gun.
    if tlen2 > AIM_DEADZONE * AIM_DEADZONE && playing {
        let tlen = tlen2.sqrt();
        ax = tx / tlen;
        az = tz / tlen;
    }
    world.set_state("aimx", ax);
    world.set_state("aimz", az);

    // --- the player's body (M36) --------------------------------------------
    // One skinned mesh where a cylinder and a sphere used to be. The *whole*
    // model turns to the aim, legs included: M30 keeps joints read-only for
    // M21's reason (a script-settable joint is hidden state), so turning only
    // a torso is not available — and a top-down shooter reads as aiming with
    // the whole body anyway.
    let model_y = py - PLAYER_SOLE;
    world.set_position("PlayerModel", px, model_y, pz);
    world.look_at("PlayerModel", px + ax * 10.0, model_y, pz + az * 10.0);

    // Idle or Run, as a **hard cut** — which is the design and not a
    // limitation. M9 rejected blending, M30 and M32 restated the rejection, and
    // `set_animation_clip` is what makes "a gait change is a different clip"
    // something a game can actually say. The setter is a no-op when the clip
    // is already the one asked for, so this costs a comparison per step.
    let moving = mlen2 > 0.0001;
    let clip = SOLDIER + "#Idle";
    if moving { clip = SOLDIER + "#Run"; }
    world.set_animation_clip("PlayerModel", clip);
    // Stride-driven while running (M32), clock-driven while idle: the legs are
    // paced by the ground actually covered, so walking into a wall does not
    // moonwalk. A breathing sway has no ground under it and wants the clock.
    let stride = 0.0;
    if moving { stride = PLAYER_STRIDE; }
    world.set_animation_stride("PlayerModel", stride);

    // --- the weapon ---------------------------------------------------------
    // Hung off the rig's right hand, which is M30's sanctioned pattern and the
    // first use of it in the repo. Read *after* the model has been placed and
    // turned this step, because `world.joint_position` composes the entity's
    // live Transform — so the muzzle is where the picture puts it.
    let was_weapon = world.state("weapon", 0).to_int();
    let weapon = was_weapon;
    if k_one { weapon = 0; }
    if k_two { weapon = 1; }
    if k_three { weapon = 2; }
    if !playing { weapon = was_weapon; }
    let switched = weapon != was_weapon;
    world.set_state("weapon", weapon);

    let held = wep_entity(weapon);
    let hand = world.joint_position("PlayerModel", "HandR");
    let hx = hand[0];
    let hy = hand[1];
    let hz = hand[2];
    world.set_position(held, hx, hy, hz);
    world.look_at(held, hx + ax * 10.0, hy, hz + az * 10.0);
    stow_weapons(world, weapon);

    let reach = wep_muzzle(weapon);
    let mux = hx + ax * reach;
    let muy = hy;
    let muz = hz + az * reach;
    let smoke_x = mux + ax * 10.0;
    let smoke_z = muz + az * 10.0;
    world.set_position("MuzzleSmoke", mux, muy, muz);
    world.look_at("MuzzleSmoke", smoke_x, muy, smoke_z);
    world.set_position("MuzzleFlash", mux, muy, muz);

    // --- firing -------------------------------------------------------------
    let mag = wep_mag(weapon);
    let ammo = world.state("ammo", mag).to_int();
    // Switching weapons swaps in a full magazine and cancels a reload: the
    // alternative is per-weapon ammo state, which is three more keys and a
    // rule about what a pickup means — and there are no pickups.
    if switched { ammo = mag; }
    let cooldown = world.state("cooldown", 0.0) - gdt;
    let was_reloading = world.state("reload", 0.0);
    let reload = was_reloading - gdt;
    if switched { reload = 0.0; }
    let finished = false;
    if reload <= 0.0 { finished = was_reloading > 0.0; }
    if finished { ammo = mag; }

    let start_reload = k_reload;
    if !playing { start_reload = false; }
    if reload > 0.0 { start_reload = false; }
    if ammo >= mag { start_reload = false; }
    if ammo <= 0 { start_reload = reload <= 0.0; }
    if start_reload { reload = wep_reload(weapon); }

    let want_fire = m_fire;
    if !playing { want_fire = false; }
    // The click that dismissed a menu is not a shot: without this, pressing
    // PLAY empties a round into the arena on the same step.
    if hit > 0 { want_fire = false; }
    if cooldown > 0.0 { want_fire = false; }
    if reload > 0.0 { want_fire = false; }
    if ammo <= 0 { want_fire = false; }

    let fired = false;
    if want_fire {
        // The single-bolt weapons' cone walks with the shot count — a fixed
        // pattern, not a random one, for the reason at the top of `spray`.
        let shots = world.state("shots", 0).to_int();
        let walk = (shots % 3).to_float() - 1.0;
        let launched = spray(
            world, wep_pellets(weapon), BULLET_COUNT, mux, muy, muz, ax, az,
            BULLET_SPEED, BULLET_LIFE, wep_spread(weapon), walk, wep_damage(weapon)
        );
        if launched > 0 {
            ammo -= 1;
            cooldown = wep_interval(weapon);
            fired = true;
            world.set_state("shots", shots + 1);
        }
    }
    world.set_state("ammo", ammo);
    world.set_state("cooldown", clampf(cooldown, -1.0, 10.0));
    world.set_state("reload", clampf(reload, 0.0, 10.0));

    // --- drones, and where they are -----------------------------------------
    // Positions are read once into arrays: the bullet loop needs every drone,
    // and `world.position` on a broken entity is a runtime error, so reading
    // through the alive flags happens in exactly one place.
    //
    // The arrays hold *this level's* ten. `i` is the index within the level and
    // `base + i + 1` is the entity's number, which is the only place the two
    // ever have to be converted: everything downstream of here — the hit test,
    // the blast, the wave counter — works in local indices, so the campaign
    // costs those loops nothing.
    let wave = world.state("wave", 1).to_int();
    let alive = [];
    let dxs = [];
    let dys = [];
    let dzs = [];
    let live_count = 0;
    for i in 0..DRONE_COUNT {
        let n = base + i + 1;
        let flag = world.state("d" + n.to_string() + "a", 1);
        let is_alive = flag > 0.5;
        alive.push(is_alive);
        dxs.push(0.0);
        dys.push(-999.0);
        dzs.push(0.0);
        if !is_alive { continue; }
        let q = world.position(drone_name(n));
        dxs[i] = q[0];
        dys[i] = q[1];
        dzs[i] = q[2];
        live_count += 1;
    }

    // --- bullets ------------------------------------------------------------
    let hit_x = 0.0;
    let hit_y = 0.0;
    let hit_z = 0.0;
    let any_hit = false;
    let blast_x = 0.0;
    let blast_y = 0.0;
    let blast_z = 0.0;
    let any_blast = false;
    let score = world.state("score", 0).to_int();
    let kills = world.state("kills", 0).to_int();

    for i in 0..BULLET_COUNT {
        let key = "b" + i.to_string();
        let life = world.state(key + "t", 0.0);
        if life <= 0.0 { continue; }

        let name = "Bullet" + pad2(i);
        let b = world.position(name);
        let bx = b[0];
        let by = b[1];
        let bz = b[2];
        let bvx = world.state(key + "x", 0.0);
        let bvz = world.state(key + "z", 0.0);
        let bdmg = world.state(key + "d", 34.0);
        let nx = bx + bvx * gdt;
        let ny = by;
        let nz = bz + bvz * gdt;
        let spent = false;

        // Drones first: a bolt that reaches one stops there.
        let j = hit_drone(alive, dxs, dys, dzs, bx, by, bz, nx, ny, nz, HIT_R2);
        if j >= 0 {
            spent = true;
            any_hit = true;
            hit_x = dxs[j];
            hit_y = dys[j];
            hit_z = dzs[j];
            let n = base + j + 1;
            let died = damage_drone(world, n, bdmg, drone_hp);
            if died > 0 {
                alive[j] = false;
                dys[j] = -999.0;
                live_count -= 1;
                kills += 1;
                score += kill_score.to_int();
            }
        }

        // Then barrels. A hit opens one, and the blast does the rest.
        let bnum = -1;
        if !spent {
            bnum = hit_barrel(world, 1, keg_live, bx, by, bz, nx, ny, nz, BARREL_R2);
        }
        if bnum > 0 {
            spent = true;
            any_blast = true;
            let bar = world.position(barrel_name(bnum));
            blast_x = bar[0];
            blast_y = bar[1];
            blast_z = bar[2];
            let killed = detonate(
                world, bnum, 1, keg_live, base,
                alive, dxs, dys, dzs, BLAST_R2, BLAST_IMPULSE
            );
            for idx in killed {
                alive[idx] = false;
                dys[idx] = -999.0;
            }
            let n_killed = killed.len();
            live_count -= n_killed;
            kills += n_killed;
            score += kill_score.to_int() * n_killed;
            score += BARREL_SCORE;
        }

        // The perimeter stops what the drones did not.
        let out = false;
        if nx < -ARENA_HALF { out = true; }
        if nx > ARENA_HALF { out = true; }
        if nz < -ARENA_HALF { out = true; }
        if nz > ARENA_HALF { out = true; }
        if out { spent = true; }
        if out {
            any_hit = true;
            hit_x = clampf(nx, -ARENA_HALF, ARENA_HALF);
            hit_y = ny;
            hit_z = clampf(nz, -ARENA_HALF, ARENA_HALF);
        }

        let left = life - gdt;
        let retire = spent;
        if left <= 0.0 { retire = true; }
        if retire {
            world.set_state(key + "t", 0.0);
            world.set_position(name, 0.0, -30.0, 0.0);
        }
        if !retire {
            world.set_state(key + "t", left);
            world.set_position(name, nx, ny, nz);
        }
    }

    world.set_state("score", score);
    world.set_state("kills", kills);

    // --- drone behaviour ----------------------------------------------------
    let hp = world.state("hp", 100.0);
    // Clearing a level hands back health rather than all of it: a campaign that
    // heals you to full has three independent fights in it, and one that heals
    // you not at all is decided by level 1.
    if advance { hp = clampf(hp + HEAL_ON_LEVEL, 0.0, 100.0); }
    let contact = false;
    for i in 0..DRONE_COUNT {
        if !alive[i] { continue; }
        let n = base + i + 1;
        let dormant = drone_wave(i + 1) > wave;
        if !playing { dormant = true; }
        if dormant {
            world.set_linear_velocity(drone_name(n), 0.0, 0.0, 0.0);
            continue;
        }
        let ex = dxs[i];
        let ey = dys[i];
        let ez = dzs[i];
        let touched = drive_drone(
            world, n, ex, ey, ez, px, py, pz, drone_speed, DRONE_HOVER, TOUCH_RANGE
        );
        if touched > 0 { contact = true; }
    }

    if !playing { contact = false; }
    if contact {
        hp -= touch_dps * gdt;
        if hp <= 0.0 {
            hp = 0.0;
            world.set_state("dead", 1);
        }
    }
    world.set_state("hp", hp);

    // --- waves --------------------------------------------------------------
    // A wave is over when nothing in it, or before it, is still flying.
    let wave_clear = true;
    for i in 0..DRONE_COUNT {
        if !alive[i] { continue; }
        if drone_wave(i + 1) <= wave { wave_clear = false; }
    }
    if !playing { wave_clear = false; }
    if wave_clear {
        // Nothing left in this level ends it; otherwise the next wave wakes
        // up. Checked in that order so a blast that takes every drone at once
        // ends the level on the wave it happened on, instead of advancing the
        // counter past waves that will never arrive.
        let more = level < LEVELS;
        if live_count == 0 && more {
            world.set_state("cleared", 1);
        } else if live_count == 0 {
            world.set_state("won", 1);
        } else if wave < WAVES {
            wave += 1;
        }
    }
    world.set_state("wave", wave);

    // This level's barrels come down and stay down. Not gated on `playing`,
    // deliberately: a barrel dropping mid-flight when the player pauses would
    // hang in the air, and holding one at rest costs the same call either way.
    settle_barrels(world, 1, keg_live, BARREL_REST);

    // The wave announcement. `shown` is the wave the banner last named, so a
    // wave that starts — including the first, when the run begins on wave 1 —
    // is exactly "the counter moved since the banner last spoke".
    let banner = world.state("banner", 0.0) - gdt;
    let shown = world.state("shown", 0).to_int();
    let told = world.state("told", 0).to_int();
    let announce = playing && wave != shown;
    // A new level outranks a new wave: they happen on the same step, and
    // "LEVEL 2" is the thing the player needs to read.
    if announce {
        world.set_hud_text("WaveBanner", "WAVE " + wave.to_string());
        banner = BANNER_TIME;
        shown = wave;
    }
    if playing && level != told {
        world.set_hud_text("WaveBanner", "LEVEL " + level.to_string());
        banner = BANNER_TIME;
        shown = wave;
        told = level;
    }
    world.set_state("told", told);
    let show_banner = banner > 0.0;
    if !playing { show_banner = false; }
    world.set_hud_visible("WaveBanner", show_banner);
    world.set_state("banner", clampf(banner, 0.0, 4.0));
    world.set_state("shown", shown);

    // --- effects ------------------------------------------------------------
    let flash = world.state("flash", 0.0);
    if fired { flash = 9.0; }
    let flash_decay = flash * gdt * 16.0 + gdt * 2.0;
    flash = clampf(flash - flash_decay, 0.0, 60.0);
    world.set_light_intensity("MuzzleFlash", flash);
    world.set_state("flash", flash);

    let smoke = world.state("smoke", 0.0);
    if fired { smoke = 0.06; }
    smoke = clampf(smoke - gdt, 0.0, 1.0);
    let smoke_rate = 0.0;
    if smoke > 0.0 { smoke_rate = 150.0; }
    // A frozen clock never runs an effect timer down, so an emitter caught
    // mid-puff by a pause or an end card would spray at full rate for as long
    // as the card is up.
    if !playing { smoke_rate = 0.0; }
    world.set_particle_rate("MuzzleSmoke", smoke_rate);
    world.set_state("smoke", smoke);

    let spark = world.state("spark", 0.0);
    if any_hit {
        spark = 0.05;
        world.set_position("Sparks", hit_x, hit_y, hit_z);
    }
    spark = clampf(spark - gdt, 0.0, 1.0);
    let spark_rate = 0.0;
    if spark > 0.0 { spark_rate = 500.0; }
    if !playing { spark_rate = 0.0; }
    world.set_particle_rate("Sparks", spark_rate);
    world.set_state("spark", spark);

    let boom = world.state("boom", 0.0);
    let boomlight = world.state("boomlight", 0.0);
    if any_blast {
        boom = 0.14;
        boomlight = 85.0;
        let smoke_y = blast_y + 0.4;
        let light_y = blast_y + 1.2;
        world.set_position("Blast", blast_x, smoke_y, blast_z);
        world.set_position("BlastLight", blast_x, light_y, blast_z);
    }
    boom = clampf(boom - gdt, 0.0, 1.0);
    let boom_rate = 0.0;
    if boom > 0.0 { boom_rate = 620.0; }
    if !playing { boom_rate = 0.0; }
    world.set_particle_rate("Blast", boom_rate);
    let light_decay = boomlight * gdt * 6.0 + gdt * 8.0;
    boomlight = clampf(boomlight - light_decay, 0.0, 400.0);
    world.set_light_intensity("BlastLight", boomlight);
    world.set_state("boom", boom);
    world.set_state("boomlight", boomlight);

    // --- camera -------------------------------------------------------------
    // Follows on a spring so the frame does not snap on every footstep, and
    // leans toward the aim so what is being shot at is on screen.
    let want_x = px + ax * CAM_LEAD;
    let want_z = pz + az * CAM_LEAD + CAM_BACK;
    let want_y = py + CAM_HEIGHT;
    let e = world.position("Eye");
    let follow = clampf(gdt * CAM_FOLLOW, 0.0, 1.0);
    let cx = e[0];
    let cy = e[1];
    let cz = e[2];
    let eye_x = cx + (want_x - cx) * follow;
    let eye_y = cy + (want_y - cy) * follow;
    let eye_z = cz + (want_z - cz) * follow;
    world.set_position("Eye", eye_x, eye_y, eye_z);

    // --- HUD ----------------------------------------------------------------
    // Hidden before the run starts: a title screen announcing SCORE 0 and
    // WAVE 1/3 is reporting on a game nobody has begun. The scene authors all
    // four hidden, so the file's own first frame *is* the title screen.
    //
    // The HUD setting picks how much of it comes back. MINIMAL keeps the two
    // readouts a fight actually needs — health and ammo — and drops the score
    // and the wave counter, which are between-fight information.
    let started = begun;
    let full_hud = started && s_hud == 0;
    let any_hud = started && s_hud < 2;
    world.set_hud_visible("ScoreGroup", full_hud);
    world.set_hud_visible("WaveText", full_hud);
    world.set_hud_visible("HealthGroup", any_hud);
    world.set_hud_visible("AmmoText", any_hud);

    let level_line = "LEVEL " + level.to_string();
    level_line += " OF " + LEVELS.to_string();
    world.set_hud_text("LevelText", level_line);
    world.set_hud_text("ScoreText", "SCORE " + score.to_string());

    let wave_line = "WAVE " + wave.to_string();
    wave_line += "/" + WAVES.to_string();
    wave_line += "   LEFT " + live_count.to_string();
    world.set_hud_text("WaveText", wave_line);

    // The gauge is a rect inside a panel whose padding is its bezel, so the
    // only number here is the health itself. It fades green to red on the way
    // down — three lines of `set_hud_color`, where a second bar behind it
    // would have been a component and an offset to keep in agreement.
    let fraction = clampf(hp / 100.0, 0.0, 1.0);
    let fill = clampf(HEALTH_FILL * fraction, 1.0, HEALTH_FILL);
    world.set_hud_rect_size("HealthFill", fill, 14.0);
    let tint = health_color(fraction);
    world.set_hud_color("HealthFill", tint[0], tint[1], tint[2]);

    // The ammo readout names the weapon, since there are three of them now and
    // "6/6" alone does not say which gun is empty.
    let ammo_line = wep_label(weapon) + " RELOADING";
    if reload <= 0.0 {
        ammo_line = wep_label(weapon) + " " + ammo.to_string();
        ammo_line += "/" + mag.to_string();
    }
    world.set_hud_text("AmmoText", ammo_line);
    // Amber while the magazine is being changed, red on the last quarter of
    // it. A quarter rather than the three rounds it used to be, because three
    // of a shotgun's six is half a magazine and three of a rifle's thirty is
    // nearly nothing — the same warning has to mean the same thing on all
    // three guns.
    let ammo_r = 1.0;
    let ammo_g = 0.95;
    let ammo_b = 0.8;
    if ammo * 4 <= mag {
        ammo_g = 0.35;
        ammo_b = 0.25;
    }
    if reload > 0.0 {
        ammo_g = 0.7;
        ammo_b = 0.3;
    }
    world.set_hud_color("AmmoText", ammo_r, ammo_g, ammo_b);

    // --- the menu -----------------------------------------------------------
    // Which words are on screen, and nothing else: the card's size, its
    // position, every button's rectangle and the hover highlight all belong to
    // the components. A screen is a list of labels; an empty one hides its slot
    // and the card closes up around the gap.
    if mode == 0 {
        let load_label = "";
        if has_save { load_label = "LOAD GAME"; }
        paint(
            world, "ARENA SHOOTER",
            "WASD MOVE   MOUSE AIM   CLICK FIRE   1 2 3 WEAPON   R RELOAD   ESC PAUSE",
            ["PLAY", "SETTINGS", load_label, "QUIT"], SLOTS
        );
    }
    if mode == 1 { world.set_hud_visible("MenuRoot", false); }
    if mode == 2 {
        paint(
            world, "PAUSED", "ESC RESUMES TOO",
            ["RESUME", "SAVE GAME", "SETTINGS", "QUIT"], SLOTS
        );
    }
    // Settings. Six rows and a BACK, each row cycling its own value — which is
    // what the seven slots in the scene are sized for.
    if mode == 5 {
        let rows = [];
        rows.push(row("DIFFICULTY", opt3(s_diff, "NORMAL", "HARD", "BRUTAL")));
        rows.push(row("CROSSHAIR", opt3(s_cross, "ON", "OFF", "OFF")));
        rows.push(row("HUD", opt3(s_hud, "FULL", "MINIMAL", "OFF")));
        rows.push(row("LIGHTS", opt3(s_light, "OFF", "LOW", "FULL")));
        rows.push(row("SHADOWS", opt3(s_shadow, "ON", "OFF", "OFF")));
        rows.push(row("QUALITY", opt3(s_qual, "LOW", "HIGH", "HIGH")));
        rows.push("BACK");
        paint(world, "SETTINGS", "CLICK A ROW TO CHANGE IT", rows, SLOTS);
    }
    // Between levels: the arena stands still and the card offers the next one.
    if mode == 4 {
        let done_line = "SCORE " + score.to_string();
        done_line += "   NEXT: FASTER, TOUGHER, LESS COVER";
        paint(
            world, "LEVEL " + level.to_string() + " CLEARED", done_line,
            ["NEXT LEVEL"], SLOTS
        );
    }
    if mode == 3 {
        let over_title = "ARENA CLEARED";
        if dead > 0.5 { over_title = "GAME OVER"; }
        let over_line = "LEVEL " + level.to_string();
        over_line += " OF " + LEVELS.to_string();
        over_line += "   SCORE " + score.to_string();
        // No buttons, and for a reason that survives the campaign: this is
        // either the last level cleared or the player dead, and both want the
        // broken drones and the spent barrels back — which the engine cannot
        // do. Every slot hides and the card closes up around them.
        paint(world, over_title, over_line, [], SLOTS);
    }

    // The reticle rides the cursor while the game is running, and is put away
    // for a menu — which has the window's own pointer.
    let show_cross = playing && s_cross == 0;
    world.set_hud_visible("Crosshair", show_cross);
    world.set_hud_offset("Crosshair", cur_x - CROSSHAIR * 0.5, cur_y - CROSSHAIR * 0.5);
}
