use rand::Rng;

//The function we are testing
fn find_uuid(data: &[u8]) -> Option<u16>
{
    if data.is_empty() {
        return None;
    }
    if data[1] != 0x03 {
        return find_uuid(&data[data[0] as usize + 1..]);
    }
    else {
        return Some(u16::from_le_bytes([data[2], data[3]]));
    }
}

fn generate_random_packet(include_uuid: bool) -> (Vec<u8>, Option<u16>) {
    let mut rng = rand::thread_rng();
    let mut packet = Vec::new();
    let mut inserted_uuid = None;

    for _ in 0..rng.gen_range(1..4) {
        let len = rng.gen_range(2..10);
        packet.push(len as u8); //Length
        packet.push(rng.gen_range(0x08..0xFF));
        for _ in 0..(len - 1) {
            packet.push(rng.gen());
        }
    }

    
    if include_uuid {
        let ad_type = if rng.gen_bool(0.5) { 0x02 } else { 0x03 };
        let uuid = rng.gen::<u16>(); //Generate random UUID
        inserted_uuid = Some(uuid);
        
        if rng.gen_bool(0.5) {
            packet.push(5);
            packet.push(ad_type);
            let junk = rng.gen::<u16>();
            packet.push((junk & 0xFF) as u8);
            packet.push(((junk >> 8) & 0xFF) as u8);
            packet.push((uuid & 0xFF) as u8);
            packet.push(((uuid >> 8) & 0xFF) as u8);
        } else {
            packet.push(3); //Length (Type + 1 UUID)
            packet.push(ad_type);
            packet.push((uuid & 0xFF) as u8);
            packet.push(((uuid >> 8) & 0xFF) as u8);
        }
    }

    //Add more stuff
    if rng.gen_bool(0.3) {
        packet.push(2);
        packet.push(0x01);
        packet.push(0x06);
    }

    (packet, inserted_uuid)
}

fn main() {
    let iterations = 100;
    let mut passed = 0;

    println!("Starting stress test: {} iterations", iterations);

    for i in 0..iterations {
        let should_have_uuid = i % 2 == 0;
        let (packet, expected_uuid) = generate_random_packet(should_have_uuid);
        
        let result = find_uuid(&packet);

        match (result, expected_uuid) {
            (Some(found), Some(expected)) if found == expected => passed += 1,
            (None, None) => passed += 1,
            (Some(found), Some(expected)) => {
                println!("Iteration {}: Found wrong UUID: {:04X}, expected: {:04X}", i, found, expected);
            },
            (Some(found), None) => {
                println!("Iteration {}: Found UUID {:04X} when none was expected!", i, found);
            },
            (None, Some(expected)) => {
                println!("Iteration {}: Failed to find UUID {:04X} in packet: {:02X?}", i, expected, packet);
            }
        }
    }

    println!("Test complete. Passed: {}/{}", passed, iterations);
}