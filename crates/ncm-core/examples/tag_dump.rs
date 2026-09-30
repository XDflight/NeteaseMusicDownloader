//! Dump every tag item of an audio file (debugging aid).
use lofty::file::TaggedFileExt;
use lofty::tag::TagExt;

fn main() {
    let path = std::env::args().nth(1).expect("usage: tag_dump <file>");
    let file = lofty::read_from_path(&path).expect("readable audio file");
    println!("type: {:?}", file.file_type());
    for tag in file.tags() {
        println!("tag {:?}: {} items, {} pictures", tag.tag_type(), tag.len(), tag.picture_count());
        for item in tag.items() {
            let v = format!("{:?}", item.value());
            println!("  {:?} = {}", item.key(), &v[..v.len().min(90)]);
        }
    }
}
