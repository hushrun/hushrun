//! The store (B-13): blocks, hints and suspensions on disk. The only code that moves a block
//! between writing, stored, expired and gone; every move is a rename.
