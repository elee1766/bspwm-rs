use std::error::Error;

use xcb::x;

fn channel(pixel: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let maximum = mask >> shift;
    u8::try_from(((pixel & mask) >> shift) * 255 / maximum)
        .expect("scaled color channel is at most 255")
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args().skip(1);
    let x_position: i16 = arguments.next().ok_or("usage: x_pixel X Y")?.parse()?;
    let y_position: i16 = arguments.next().ok_or("usage: x_pixel X Y")?.parse()?;
    if arguments.next().is_some() {
        return Err("usage: x_pixel X Y".into());
    }

    let (connection, screen_number) = xcb::Connection::connect(None)?;
    let setup = connection.get_setup();
    let screen = setup
        .roots()
        .nth(usize::try_from(screen_number)?)
        .ok_or("X server has no selected screen")?;
    let visual = setup
        .roots()
        .flat_map(|root| root.allowed_depths())
        .flat_map(x::Depth::visuals)
        .find(|visual| visual.visual_id() == screen.root_visual())
        .ok_or("root visual is missing")?;
    let bits_per_pixel = setup
        .pixmap_formats()
        .iter()
        .find(|format| format.depth() == screen.root_depth())
        .map_or(32, x::Format::bits_per_pixel);

    let cookie = connection.send_request(&x::GetImage {
        format: x::ImageFormat::ZPixmap,
        drawable: x::Drawable::Window(screen.root()),
        x: x_position,
        y: y_position,
        width: 1,
        height: 1,
        plane_mask: u32::MAX,
    });
    let reply = connection.wait_for_reply(cookie)?;
    let byte_count = usize::from(bits_per_pixel.div_ceil(8)).min(4);
    let data = reply
        .data()
        .get(..byte_count)
        .ok_or("pixel reply is empty")?;
    let mut bytes = [0_u8; 4];
    bytes[..byte_count].copy_from_slice(data);
    let pixel = match setup.image_byte_order() {
        x::ImageOrder::LsbFirst => u32::from_le_bytes(bytes),
        x::ImageOrder::MsbFirst => u32::from_be_bytes(bytes),
    };

    println!(
        "#{:02X}{:02X}{:02X}",
        channel(pixel, visual.red_mask()),
        channel(pixel, visual.green_mask()),
        channel(pixel, visual.blue_mask()),
    );
    Ok(())
}
