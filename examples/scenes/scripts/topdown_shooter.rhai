// Arena Shooter — the whole game, in one script.
//
//   WASD    move          (world-relative, the way a top-down game moves)
//   Arrows  aim           (independent of movement — twin-stick, on a keyboard)
//   Space   fire
//   R       reload
//
// Three waves of hovering drones home in on the player. Shoot them, or shoot a
// barrel and let the blast take everything near it. Health runs out, the run
// ends; clear all ten and it says so.
//
// How the pieces fit, since none of it is obvious from the scene file:
//
// * `Player` is a physics proxy with no mesh. A dynamic body owns its own
//   Transform, so a script cannot turn one to face the aim — `PlayerBody`,
//   `PlayerHead` and `PlayerGun` are separate visual entities placed from the
//   proxy's position every step. That is M12's wheel pattern.
// * Bullets are not physics. Fourteen mesh-only entities are recycled here,
//   flown at BULLET_SPEED, and tested against drone centres as a swept
//   *segment* — a 46 m/s round covers 0.77 m per step, so a point test would
//   shoot straight through a drone between two frames. Nothing about them
//   touches rapier, which is also why a hail of fire cannot disturb the arena.
//   Known limit: they pass over cover. Cover stops bodies, not bolts.
// * All run state is `world.state` — numeric, per-run, replay-deterministic,
//   and deliberately not baked. Entity names are built from indices, which is
//   why the counts below must match `make_arena.py`.
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
// replays to the same pixels.

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

// Which wave a drone belongs to. Waves are contiguous index ranges; the
// generator prints these boundaries when it writes the scene.
fn drone_wave(n) {
    if n <= 4 { return 1; }
    if n <= 7 { return 2; }
    3
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
fn hit_barrel(world, count, bx, by, bz, nx, ny, nz, r2) {
    let found = -1;
    for j in 0..count {
        if found >= 0 { continue; }
        let n = j + 1;
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
fn detonate(world, n, barrel_count, alive, dxs, dys, dzs, r2, impulse) {
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
        let m = k + 1;
        let dname = drone_name(m);
        world.break_entity(dname);
        world.set_state("d" + m.to_string() + "a", 0);
        killed.push(k);
    }

    for k in 0..barrel_count {
        let m = k + 1;
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

fn step(world, step) {
    let dt = world.dt();

    // --- constants, matching make_arena.py ---------------------------------
    let DRONE_COUNT = 10;
    let BULLET_COUNT = 14;
    let BARREL_COUNT = 4;
    let ARENA_HALF = 28.0;
    let WAVES = 3;

    let MOVE_SPEED = 7.2;        // m/s
    let MOVE_ACCEL = 16.0;       // 1/s, how fast velocity reaches the target
    let AIM_TURN = 14.0;         // 1/s, how fast the gun swings to a new aim
    let BULLET_SPEED = 46.0;     // m/s
    let BULLET_LIFE = 1.05;      // s
    let HIT_R2 = 0.9;            // m^2, bolt-vs-drone (~0.95 m)
    let BARREL_R2 = 1.1;         // m^2, bolt-vs-barrel
    let FIRE_INTERVAL = 0.13;    // s
    let MAG_SIZE = 12;
    let RELOAD_TIME = 1.3;       // s
    let MUZZLE_HEIGHT = 1.15;    // m above the player's origin
    let MUZZLE_FORWARD = 1.05;   // m

    let DRONE_SPEED = 4.4;       // m/s
    let DRONE_HOVER = 0.95;      // m, the altitude they hold
    let DRONE_HP = 100.0;
    let BULLET_DAMAGE = 34.0;    // three bolts to a kill
    let TOUCH_RANGE = 1.3;       // m, where a drone starts hurting
    let TOUCH_DPS = 26.0;
    let DRONE_SCORE = 100;

    let BLAST_R2 = 56.25;        // (7.5 m)^2
    let BLAST_IMPULSE = 260.0;
    let BARREL_SCORE = 50;

    let CAM_HEIGHT = 20.0;
    let CAM_BACK = 11.5;
    let CAM_LEAD = 2.5;          // m the view leans toward the aim
    let CAM_FOLLOW = 7.0;        // 1/s

    let dead = world.state("dead", 0);
    let won = world.state("won", 0);
    let playing = true;
    if dead > 0.5 { playing = false; }
    if won > 0.5 { playing = false; }

    // Input is read once, unconditionally, and then discarded if the run is
    // over. Reading it inside a branch would nest another block for nothing.
    let k_w = world.key("KeyW");
    let k_s = world.key("KeyS");
    let k_a = world.key("KeyA");
    let k_d = world.key("KeyD");
    let k_up = world.key("ArrowUp");
    let k_down = world.key("ArrowDown");
    let k_left = world.key("ArrowLeft");
    let k_right = world.key("ArrowRight");
    let k_fire = world.key("Space");
    let k_reload = world.key("KeyR");

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
    let blend = clampf(dt * MOVE_ACCEL, 0.0, 1.0);
    let want_vx = mx * MOVE_SPEED;
    let want_vz = mz * MOVE_SPEED;
    let vx = v[0] + (want_vx - v[0]) * blend;
    let vz = v[2] + (want_vz - v[2]) * blend;
    world.set_linear_velocity("Player", vx, vy, vz);

    // --- aim ----------------------------------------------------------------
    // Kept as a unit vector rather than an angle: `look_at` takes a point, so
    // nothing here needs an inverse tangent, and there is no wrap to get wrong.
    let ax = world.state("aimx", 0.0);
    let az = world.state("aimz", -1.0);
    let alen2 = ax * ax + az * az;
    if alen2 < 0.0001 {
        ax = 0.0;
        az = -1.0;
    }

    let tx = 0.0;
    let tz = 0.0;
    if k_up { tz -= 1.0; }
    if k_down { tz += 1.0; }
    if k_left { tx -= 1.0; }
    if k_right { tx += 1.0; }
    if !playing {
        tx = 0.0;
        tz = 0.0;
    }
    let tlen2 = tx * tx + tz * tz;
    if tlen2 > 0.0001 {
        let tlen = tlen2.sqrt();
        tx /= tlen;
        tz /= tlen;
        let turn = clampf(dt * AIM_TURN, 0.0, 1.0);
        ax += (tx - ax) * turn;
        az += (tz - az) * turn;
        let nlen = (ax * ax + az * az).sqrt();
        ax /= nlen;
        az /= nlen;
    }
    world.set_state("aimx", ax);
    world.set_state("aimz", az);

    // --- the player's visible parts -----------------------------------------
    let gun_y = py + MUZZLE_HEIGHT - 0.55;
    let gx = px + ax * 0.55;
    let gz = pz + az * 0.55;
    let body_y = py - 0.12;
    let head_y = py + 0.62;
    let head_x = px + ax * 0.06;
    let head_z = pz + az * 0.06;
    world.set_position("PlayerBody", px, body_y, pz);
    world.set_position("PlayerHead", head_x, head_y, head_z);
    world.set_position("PlayerGun", gx, gun_y, gz);
    let look_x = gx + ax * 10.0;
    let look_z = gz + az * 10.0;
    world.look_at("PlayerGun", look_x, gun_y, look_z);

    let mux = px + ax * MUZZLE_FORWARD;
    let muy = py + MUZZLE_HEIGHT - 0.28;
    let muz = pz + az * MUZZLE_FORWARD;
    let smoke_x = mux + ax * 10.0;
    let smoke_z = muz + az * 10.0;
    world.set_position("MuzzleSmoke", mux, muy, muz);
    world.look_at("MuzzleSmoke", smoke_x, muy, smoke_z);
    world.set_position("MuzzleFlash", mux, muy, muz);

    // --- the gun ------------------------------------------------------------
    let ammo = world.state("ammo", MAG_SIZE).to_int();
    let cooldown = world.state("cooldown", 0.0) - dt;
    let was_reloading = world.state("reload", 0.0);
    let reload = was_reloading - dt;
    let finished = false;
    if reload <= 0.0 { finished = was_reloading > 0.0; }
    if finished { ammo = MAG_SIZE; }

    let start_reload = k_reload;
    if !playing { start_reload = false; }
    if reload > 0.0 { start_reload = false; }
    if ammo >= MAG_SIZE { start_reload = false; }
    if ammo <= 0 { start_reload = reload <= 0.0; }
    if start_reload { reload = RELOAD_TIME; }

    let want_fire = k_fire;
    if !playing { want_fire = false; }
    if cooldown > 0.0 { want_fire = false; }
    if reload > 0.0 { want_fire = false; }
    if ammo <= 0 { want_fire = false; }

    let fired = false;
    if want_fire {
        // Find a free slot. Scanning beats a round-robin index: it never
        // overwrites a bolt still in flight.
        let slot = -1;
        for i in 0..BULLET_COUNT {
            let life = world.state("b" + i.to_string() + "t", 0.0);
            if life > 0.0 { continue; }
            if slot < 0 { slot = i; }
        }
        if slot >= 0 {
            let key = "b" + slot.to_string();
            let name = "Bullet" + pad2(slot);
            world.set_position(name, mux, muy, muz);
            world.set_state(key + "t", BULLET_LIFE);
            world.set_state(key + "x", ax * BULLET_SPEED);
            world.set_state(key + "z", az * BULLET_SPEED);
            ammo -= 1;
            cooldown = FIRE_INTERVAL;
            fired = true;
        }
    }
    world.set_state("ammo", ammo);
    world.set_state("cooldown", clampf(cooldown, -1.0, 10.0));
    world.set_state("reload", clampf(reload, 0.0, 10.0));

    // --- drones, and where they are -----------------------------------------
    // Positions are read once into arrays: the bullet loop needs every drone,
    // and `world.position` on a broken entity is a runtime error, so reading
    // through the alive flags happens in exactly one place.
    let wave = world.state("wave", 1).to_int();
    let alive = [];
    let dxs = [];
    let dys = [];
    let dzs = [];
    let live_count = 0;
    for i in 0..DRONE_COUNT {
        let n = i + 1;
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
        let nx = bx + bvx * dt;
        let ny = by;
        let nz = bz + bvz * dt;
        let spent = false;

        // Drones first: a bolt that reaches one stops there.
        let j = hit_drone(alive, dxs, dys, dzs, bx, by, bz, nx, ny, nz, HIT_R2);
        if j >= 0 {
            spent = true;
            any_hit = true;
            hit_x = dxs[j];
            hit_y = dys[j];
            hit_z = dzs[j];
            let n = j + 1;
            let died = damage_drone(world, n, BULLET_DAMAGE, DRONE_HP);
            if died > 0 {
                alive[j] = false;
                dys[j] = -999.0;
                live_count -= 1;
                kills += 1;
                score += DRONE_SCORE;
            }
        }

        // Then barrels. A hit opens one, and the blast does the rest.
        let bnum = -1;
        if !spent {
            bnum = hit_barrel(world, BARREL_COUNT, bx, by, bz, nx, ny, nz, BARREL_R2);
        }
        if bnum > 0 {
            spent = true;
            any_blast = true;
            let bar = world.position(barrel_name(bnum));
            blast_x = bar[0];
            blast_y = bar[1];
            blast_z = bar[2];
            let killed = detonate(
                world, bnum, BARREL_COUNT, alive, dxs, dys, dzs, BLAST_R2, BLAST_IMPULSE
            );
            for idx in killed {
                alive[idx] = false;
                dys[idx] = -999.0;
            }
            let n_killed = killed.len();
            live_count -= n_killed;
            kills += n_killed;
            score += DRONE_SCORE * n_killed;
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

        let left = life - dt;
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
    let contact = false;
    for i in 0..DRONE_COUNT {
        if !alive[i] { continue; }
        let n = i + 1;
        let dormant = drone_wave(n) > wave;
        if !playing { dormant = true; }
        if dormant {
            world.set_linear_velocity(drone_name(n), 0.0, 0.0, 0.0);
            continue;
        }
        let ex = dxs[i];
        let ey = dys[i];
        let ez = dzs[i];
        let touched = drive_drone(
            world, n, ex, ey, ez, px, py, pz, DRONE_SPEED, DRONE_HOVER, TOUCH_RANGE
        );
        if touched > 0 { contact = true; }
    }

    if !playing { contact = false; }
    if contact {
        hp -= TOUCH_DPS * dt;
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
        let n = i + 1;
        if drone_wave(n) <= wave { wave_clear = false; }
    }
    if !playing { wave_clear = false; }
    if wave_clear {
        // Nothing left anywhere ends the run; otherwise the next wave wakes
        // up. Checked in that order so a blast that takes every drone at once
        // wins on the wave it happened on, instead of advancing the counter
        // past waves that will never arrive.
        if live_count == 0 {
            world.set_state("won", 1);
        } else if wave < WAVES {
            wave += 1;
        }
    }
    world.set_state("wave", wave);

    // --- effects ------------------------------------------------------------
    let flash = world.state("flash", 0.0);
    if fired { flash = 9.0; }
    let flash_decay = flash * dt * 16.0 + dt * 2.0;
    flash = clampf(flash - flash_decay, 0.0, 60.0);
    world.set_light_intensity("MuzzleFlash", flash);
    world.set_state("flash", flash);

    let smoke = world.state("smoke", 0.0);
    if fired { smoke = 0.06; }
    smoke = clampf(smoke - dt, 0.0, 1.0);
    let smoke_rate = 0.0;
    if smoke > 0.0 { smoke_rate = 150.0; }
    world.set_particle_rate("MuzzleSmoke", smoke_rate);
    world.set_state("smoke", smoke);

    let spark = world.state("spark", 0.0);
    if any_hit {
        spark = 0.05;
        world.set_position("Sparks", hit_x, hit_y, hit_z);
    }
    spark = clampf(spark - dt, 0.0, 1.0);
    let spark_rate = 0.0;
    if spark > 0.0 { spark_rate = 500.0; }
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
    boom = clampf(boom - dt, 0.0, 1.0);
    let boom_rate = 0.0;
    if boom > 0.0 { boom_rate = 620.0; }
    world.set_particle_rate("Blast", boom_rate);
    let light_decay = boomlight * dt * 6.0 + dt * 8.0;
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
    let follow = clampf(dt * CAM_FOLLOW, 0.0, 1.0);
    let cx = e[0];
    let cy = e[1];
    let cz = e[2];
    let eye_x = cx + (want_x - cx) * follow;
    let eye_y = cy + (want_y - cy) * follow;
    let eye_z = cz + (want_z - cz) * follow;
    world.set_position("Eye", eye_x, eye_y, eye_z);

    // --- HUD ----------------------------------------------------------------
    world.set_hud_text("ScoreText", "SCORE " + score.to_string());

    let wave_line = "WAVE " + wave.to_string();
    wave_line += "/" + WAVES.to_string();
    wave_line += "   LEFT " + live_count.to_string();
    world.set_hud_text("WaveText", wave_line);

    let fill = clampf(218.0 * hp / 100.0, 1.0, 218.0);
    world.set_hud_rect_size("HealthFill", fill, 14.0);

    let ammo_line = "RELOADING";
    if reload <= 0.0 {
        ammo_line = "AMMO " + ammo.to_string();
        ammo_line += "/" + MAG_SIZE.to_string();
    }
    world.set_hud_text("AmmoText", ammo_line);

    let banner = "";
    if world.state("dead", 0) > 0.5 { banner = "GAME OVER"; }
    if world.state("won", 0) > 0.5 { banner = "ARENA CLEARED"; }
    if banner != "" { world.set_hud_text("CenterText", banner); }

    let hint = world.state("hint", 1);
    if step > 420 {
        if hint > 0.5 {
            world.set_hud_text("HintText", "");
            world.set_state("hint", 0);
        }
    }
}
