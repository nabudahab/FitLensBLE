use rand::Rng;

//The function we are testing
fn find_uuid(data: &[u8]) -> Option<u16>
{
    if data.is_empty() {
        return None;
    }
    if data[1] != 0x03 && data[1] != 0x02 {
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
        
        packet.push(3); //Length (Type + 1 UUID)
        packet.push(ad_type);
        packet.push((uuid & 0xFF) as u8);
        packet.push(((uuid >> 8) & 0xFF) as u8);
    }

    //Add more stuff
    if rng.gen_bool(0.3) {
        packet.push(2);
        packet.push(0x01);
        packet.push(0x06);
    }

    (packet, inserted_uuid)
}

fn search_for_uuid(data: &[u8], uuid: u16) -> bool
{
    //convert UUID to 2 bytes in little endian
    let uuid_bytes: [u8; 2] = [(uuid & 0xFF) as u8, (uuid >> 8) as u8];
    let size = data.len();
    let mut i = 0;
    while(i < size - 2) //loops over a single packet
    {
        let mut packet_size = data[i] as usize;
        if (data[i+1] != 0x02 && data[i+1] != 0x03) {
            i += packet_size + 1;
            continue;
        } else {
            //check not only if the UUID exists, but if it starts at an "even" number of bytes from the packet start
            // so we don't accidentally mush two packets together
            if(data[i + 2..i+packet_size + 1].windows(2).position(|w| w == uuid_bytes).map(|pos| pos % 2 == 0).unwrap_or(false)){
                return true;
            }
            else {
                continue;
            }
        }
        i += packet_size + 1;
    }
    return false;
}

fn test_find_uuid(iterations: usize)
{
    let mut passed = 0;

    println!("Starting find_uuid stress test: {} iterations", iterations);

    for i in 0..iterations {
        let should_have_uuid = i % 2 == 0;
        let (packet, expected_uuid) = generate_random_packet(should_have_uuid);
        
        let result = find_uuid(&packet);

        match (result, expected_uuid) {
            (Some(found), Some(expected)) if found == expected => passed += 1,
            (None, None) => passed += 1,
            (Some(found), Some(expected)) => {
                println!("Iteration {}: Found wrong UUID: {:04X}, expected: {:04X} in packet: {:02X?}", i, found, expected, packet);
            },
            (Some(found), None) => {
                println!("Iteration {}: Found UUID {:04X} when none was expected!", i, found);
            },
            (None, Some(expected)) => {
                println!("Iteration {}: Failed to find UUID {:04X} in packet: {:02X?}", i, expected, packet);
            }
        }
    }

    println!("find_uuid test complete. Passed: {}/{}", passed, iterations);
}

fn test_search_for_uuid(iterations: usize) {
    let mut passed = 0;

    println!("Starting search_for_uuid stress test: {} iterations", iterations);

    for i in 0..iterations {
        let should_have_uuid = i % 2 == 0;
        let (packet, expected_uuid) = generate_random_packet(should_have_uuid);
        
        let result = if let Some(uuid) = expected_uuid {
            search_for_uuid(&packet, uuid)
        } else {
            !search_for_uuid(&packet, 0) //should not find a random UUID when none expected
        };

        if result {
            passed += 1;
        } else {
            match expected_uuid {
                Some(uuid) => println!("Iteration {}: Failed to find UUID {:04X} in packet: {:02X?}", i, uuid, packet),
                None => println!("Iteration {}: Found UUID in packet when none was expected: {:02X?}", i, packet),
            }
        }
    }

    println!("search_for_uuid test complete. Passed: {}/{}", passed, iterations);
}

fn main() {
    let iterations = 100_000;
    
    test_find_uuid(iterations);
    println!();
    test_search_for_uuid(iterations);
}