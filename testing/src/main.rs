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

fn generate_invalid_packet(include_uuid: bool) -> (Vec<u8>, Option<u16>) {
    let mut rng = rand::thread_rng();
    let mut packet = Vec::new();
    let mut inserted_uuid = None;
    
    // First, generate some valid packets
    for _ in 0..rng.gen_range(1..4) {
        let len = rng.gen_range(2..10);
        packet.push(len as u8);
        packet.push(rng.gen_range(0x08..0xFF));
        for _ in 0..(len - 1) {
            packet.push(rng.gen());
        }
    }
    
    // Optionally inject a valid UUID into the valid packets
    if include_uuid {
        let ad_type = if rng.gen_bool(0.5) { 0x02 } else { 0x03 };
        let uuid = rng.gen::<u16>();
        inserted_uuid = Some(uuid);
        
        packet.push(3); // Length (Type + 1 UUID)
        packet.push(ad_type);
        packet.push((uuid & 0xFF) as u8);
        packet.push(((uuid >> 8) & 0xFF) as u8);
    }
    
    // Now add an invalid packet at the very end
    let invalid_type = rng.gen_range(0..2);
    match invalid_type {
        0 => {
            // Zero length packet at end
            packet.push(0x00);
            packet.push(0x03);
            for _ in 0..rng.gen_range(1..10) {
                packet.push(rng.gen());
            }
        },
        _ => {
            // Truncated packet at end - length says more data but we cut it short
            let len = rng.gen_range(5..15);
            packet.push(len as u8);
            packet.push(rng.gen_range(0x08..0xFF));
            // Only add partial data, truncate early
            let partial = rng.gen_range(1..len.min(3));
            for _ in 0..partial {
                packet.push(rng.gen());
            }
        }
    }
    
    (packet, inserted_uuid)
}

fn generate_edge_case_packet(case_type: usize) -> (Vec<u8>, Option<u16>) {
    let mut rng = rand::thread_rng();
    let uuid = rng.gen::<u16>();
    let ad_type = if rng.gen_bool(0.5) { 0x02 } else { 0x03 };
    
    match case_type {
        0 => {
            //Raw UUID bytes at the very beginning (no length/type header) - should NOT find it
            let mut packet = vec![(uuid & 0xFF) as u8, ((uuid >> 8) & 0xFF) as u8];
            //Add some valid junk after
            for _ in 0..rng.gen_range(1..5) {
                let len = rng.gen_range(2..8);
                packet.push(len as u8);
                packet.push(rng.gen_range(0x08..0xFF));
                for _ in 0..(len - 1) {
                    packet.push(rng.gen());
                }
            }
            (packet, None) //Should NOT find this UUID
        },
        1 => {
            //UUID at the very end (no data after it)
            let mut packet = Vec::new();
            for _ in 0..rng.gen_range(1..5) {
                let len = rng.gen_range(2..8);
                packet.push(len as u8);
                packet.push(rng.gen_range(0x08..0xFF));
                for _ in 0..(len - 1) {
                    packet.push(rng.gen());
                }
            }
            packet.push(3);
            packet.push(ad_type);
            packet.push((uuid & 0xFF) as u8);
            packet.push(((uuid >> 8) & 0xFF) as u8);
            (packet, Some(uuid))
        },
        2 => {
            //Multiple UUIDs in one AD structure, we want the first one
            let uuid2 = rng.gen::<u16>();
            let uuid3 = rng.gen::<u16>();
            let packet = vec![
                7, ad_type,
                (uuid & 0xFF) as u8, ((uuid >> 8) & 0xFF) as u8,
                (uuid2 & 0xFF) as u8, ((uuid2 >> 8) & 0xFF) as u8,
                (uuid3 & 0xFF) as u8, ((uuid3 >> 8) & 0xFF) as u8,
            ];
            (packet, Some(uuid))
        },
        3 => {
            //Empty packet
            (vec![], None)
        },
        4 => {
            //Single byte packet
            (vec![0x03], None)
        },
        _ => {
            //Just length and type, no data
            (vec![3, ad_type], None)
        }
    }
}

fn search_for_uuid(data: &[u8], uuid: u16) -> bool
{
    //convert UUID to 2 bytes in little endian
    let uuid_bytes: [u8; 2] = [(uuid & 0xFF) as u8, (uuid >> 8) as u8];
    let size = data.len();
    if size < 2 {
        return false;
    }
    let mut i = 0;
    while i < size - 2 //loops over a single packet
    {
        let packet_size = data[i] as usize;
        if data[i+1] != 0x02 && data[i+1] != 0x03 {
            i += packet_size + 1;
            continue;
        } else {
            //check not only if the UUID exists, but if it starts at an "even" number of bytes from the packet start
            //so we don't accidentally mush two packets together
            let end_idx = (i + packet_size + 1).min(size);
            if end_idx > i + 2 && data[i + 2..end_idx].windows(2).position(|w| w == uuid_bytes).map(|pos| pos % 2 == 0).unwrap_or(false) {
                return true;
            }
            else {
                i += packet_size + 1;
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

fn test_find_uuid_invalid(iterations: usize) {
    let mut passed = 0;

    println!("Starting find_uuid invalid packet test: {} iterations", iterations);

    for i in 0..iterations {
        let should_have_uuid = i % 2 == 0;
        let (packet, expected_uuid) = generate_invalid_packet(should_have_uuid);
        
        let result = find_uuid(&packet);

        match (result, expected_uuid) {
            (Some(found), Some(expected)) if found == expected => passed += 1,
            (None, None) => passed += 1,
            (Some(found), Some(expected)) => {
                println!("Iteration {}: Found wrong UUID: {:04X}, expected: {:04X} in packet: {:02X?}", i, found, expected, packet);
            },
            (Some(found), None) => {
                //It's ok to find something in invalid packets, just shouldn't be what we didn't insert
                passed += 1;
            },
            (None, Some(expected)) => {
                println!("Iteration {}: Failed to find UUID {:04X} in invalid packet: {:02X?}", i, expected, packet);
            }
        }
    }

    println!("find_uuid invalid packet test complete. Passed: {}/{}", passed, iterations);
}

fn test_search_for_uuid_invalid(iterations: usize) {
    let mut passed = 0;

    println!("Starting search_for_uuid invalid packet test: {} iterations", iterations);

    for i in 0..iterations {
        let should_have_uuid = i % 2 == 0;
        let (packet, expected_uuid) = generate_invalid_packet(should_have_uuid);

        let (search_uuid, should_find) = if let Some(uuid) = expected_uuid {
            (uuid, true)
        } else {
            (0, false)
        };
        
        println!("Testing packet {:02X?}, Searching for UUID: {:04X}, Expected to find: {}", packet, search_uuid, should_find);
        
        let result = if should_find {
            search_for_uuid(&packet, search_uuid)
        } else {
            !search_for_uuid(&packet, search_uuid) //should not find a random UUID when none expected
        };

        if result {
            passed += 1;
        } else {
            match expected_uuid {
                Some(uuid) => println!("Iteration {}: Failed to find UUID {:04X} in invalid packet: {:02X?}", i, uuid, packet),
                None => println!("Iteration {}: Found UUID in packet when none was expected: {:02X?}", i, packet),
            }
        }
    }

    println!("search_for_uuid invalid packet test complete. Passed: {}/{}", passed, iterations);
}

fn test_find_uuid_edge_cases() {
    let mut passed = 0;
    let total = 6;

    println!("Starting find_uuid edge case tests: {} cases", total);

    for case in 0..total {
        let (packet, expected_uuid) = generate_edge_case_packet(case);
        let result = find_uuid(&packet);

        match (result, expected_uuid) {
            (Some(found), Some(expected)) if found == expected => {
                passed += 1;
            },
            (None, None) => {
                passed += 1;
            },
            (Some(found), Some(expected)) => {
                println!("Case {}: Found wrong UUID: {:04X}, expected: {:04X} in packet: {:02X?}", case, found, expected, packet);
            },
            (Some(found), None) => {
                println!("Case {}: Found UUID {:04X} when none was expected in packet: {:02X?}", case, found, packet);
            },
            (None, Some(expected)) => {
                println!("Case {}: Failed to find UUID {:04X} in packet: {:02X?}", case, expected, packet);
            }
        }
    }

    println!("find_uuid edge case test complete. Passed: {}/{}", passed, total);
}

fn test_search_for_uuid_edge_cases() {
    let mut passed = 0;
    let total = 6;

    println!("Starting search_for_uuid edge case tests: {} cases", total);

    for case in 0..total {
        let (packet, expected_uuid) = generate_edge_case_packet(case);
        
        let result = if let Some(uuid) = expected_uuid {
            search_for_uuid(&packet, uuid)
        } else {
            !search_for_uuid(&packet, 0)
        };

        if result {
            passed += 1;
        } else {
            match expected_uuid {
                Some(uuid) => println!("Case {}: Failed to find UUID {:04X} in packet: {:02X?}", case, uuid, packet),
                None => println!("Case {}: Found UUID in packet when none was expected: {:02X?}", case, packet),
            }
        }
    }

    println!("search_for_uuid edge case test complete. Passed: {}/{}", passed, total);
}

fn main() {
    let iterations = 100_000;
    let invalid_iterations = 10_000;
    
    // test_find_uuid(iterations);
    // println!();
    test_search_for_uuid(iterations);
    println!();
    // test_find_uuid_invalid(invalid_iterations);
    // println!();
    test_search_for_uuid_edge_cases();
    println!();
    // test_find_uuid_edge_cases();
    // println!();
    test_search_for_uuid_invalid(invalid_iterations);
}