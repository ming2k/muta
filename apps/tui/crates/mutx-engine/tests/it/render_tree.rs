use mutx_engine::render_tree::*;
use mutx_engine::{Color, Style, TestTerminal};

#[test]
fn test_flex_layout_constraints_down_size_up() {
    let child1 = Box::new(RenderLeaf::new("c1", Size::new(10, 2)));
    let child2 = Box::new(RenderLeaf::new("c2", Size::new(15, 3)));

    let mut col = RenderFlex::column()
        .gap(1)
        .cross_align(CrossAxisAlignment::Start)
        .with_child(FlexChild::fixed(child1))
        .with_child(FlexChild::fixed(child2));

    let constraints = BoxConstraints::loose(Size::new(80, 24));
    let size = col.layout(constraints);

    // Height = 2 + 1 (gap) + 3 = 6
    assert_eq!(size.height, 6);
    // Width = max(10, 15) = 15 (loose constraint)
    assert_eq!(size.width, 15);

    assert_eq!(col.children[0].render_box.offset(), Offset::new(0, 0));
    assert_eq!(col.children[1].render_box.offset(), Offset::new(0, 3));
}

#[test]
fn test_flex_grow_distribution() {
    let fixed = Box::new(RenderLeaf::new("fixed", Size::new(80, 4)));
    let grow1 = Box::new(RenderLeaf::new("grow1", Size::new(80, 0)));
    let grow2 = Box::new(RenderLeaf::new("grow2", Size::new(80, 0)));

    let mut col = RenderFlex::column()
        .with_child(FlexChild::fixed(fixed))
        .with_child(FlexChild::flex(grow1, 1))
        .with_child(FlexChild::flex(grow2, 2));

    // Tight 80x16 viewport
    let constraints = BoxConstraints::tight(Size::new(80, 16));
    let size = col.layout(constraints);

    assert_eq!(size, Size::new(80, 16));
    assert_eq!(col.children[0].render_box.size().height, 4);
    // Remaining space = 16 - 4 = 12. Ratio 1:2 -> grow1 gets 4, grow2 gets 8
    assert_eq!(col.children[1].render_box.size().height, 4);
    assert_eq!(col.children[2].render_box.size().height, 8);

    assert_eq!(col.children[0].render_box.offset(), Offset::new(0, 0));
    assert_eq!(col.children[1].render_box.offset(), Offset::new(0, 4));
    assert_eq!(col.children[2].render_box.offset(), Offset::new(0, 8));
}

#[test]
fn test_stack_alignment_and_reverse_hit_test() {
    let bg = Box::new(RenderLeaf::new("bg", Size::new(40, 20)).interactive(true));
    let dialog = Box::new(RenderLeaf::new("dialog", Size::new(20, 10)).interactive(true));

    let stack = RenderStack::expand()
        .with_child(StackChild::fill(bg))
        .with_child(StackChild::aligned(dialog, Alignment::Center));

    let mut tree = RenderTree::new(Box::new(stack));
    tree.layout(Size::new(40, 20));

    // Point in center (20, 10) hits dialog first (topmost in reverse paint order)
    let hit_center = tree.hit_test((20, 10));
    assert_eq!(hit_center.top().map(|e| e.tag), Some("dialog"));

    // Point near corner (2, 2) outside dialog hits background
    let hit_corner = tree.hit_test((2, 2));
    assert_eq!(hit_corner.top().map(|e| e.tag), Some("bg"));
}

#[test]
fn test_render_tree_paint_clipped_isolation() {
    let custom = Box::new(
        RenderCustom::new("overflowing_child")
            .on_layout(|constraints| constraints.constrain(Size::new(20, 5)))
            .on_paint(|ctx, _size| {
                // Attempts to paint 20 characters on row 0
                ctx.put(0, 0, Style::default().fg(Color::Green), "01234567890123456789");
            })
    );

    // Wrap in a clip box of width 8
    let clipped = Box::new(RenderClip::new(
        Box::new(RenderPadding::new(
            mutx_engine::Margin::new(2, 1),
            custom,
        ))
    ));

    let mut tree = RenderTree::new(clipped);
    let mut terminal = TestTerminal::new(30, 10);
    terminal.draw_tree(&mut tree);

    let grid = terminal.buffer();
    // At y=1, x should start after padding (x=2)
    assert_eq!(grid.get(0, 1).unwrap().symbol.as_str(), " ");
    assert_eq!(grid.get(1, 1).unwrap().symbol.as_str(), " ");
    assert_eq!(grid.get(2, 1).unwrap().symbol.as_str(), "0");
    assert_eq!(grid.get(3, 1).unwrap().symbol.as_str(), "1");
}

#[test]
fn test_paragraph_box_wrapping_and_measurement() {
    let mut para = RenderParagraph::text("hello world from render box");
    // Max width 12 forces wrapping
    let constraints = BoxConstraints::new(0, 12, 0, 10);
    let size = para.layout(constraints);

    assert!(size.width <= 12);
    assert!(size.height >= 2);
}

#[test]
fn test_terminal_draw_tree_promotes_and_idles_on_stable_tree() {
    use mutx_engine::backend::{Backend, Bce};
    use mutx_engine::Terminal;

    let leaf = Box::new(RenderLeaf::new("leaf", Size::new(20, 2)));
    let mut tree = RenderTree::new(leaf);

    let writer = Vec::<u8>::new();
    let backend = Backend::with_bce(writer, Bce::No);
    let mut terminal = Terminal::new(backend);

    terminal.draw_tree(&mut tree).unwrap();
    let first_len = terminal.writer().len();
    assert!(first_len > 0, "first frame flushes bytes");

    terminal.writer().clear();
    // Second draw with same tree and no invalidation -> zero bytes
    terminal.draw_tree(&mut tree).unwrap();
    assert_eq!(terminal.writer().len(), 0, "stable tree is zero-byte commit");
}

#[test]
fn test_nested_complex_composition_tree() {
    // Construct real multi-layer layout:
    // Stack:
    //   - Background fill
    //   - Column:
    //       - Header (fixed 1 row)
    //       - Content area (Flex row: sidebar 15 cols, main area flex grow)
    //       - Footer (fixed 1 row)
    let bg = Box::new(RenderLeaf::new("bg", Size::new(80, 24)).interactive(true));
    let header = Box::new(RenderLeaf::new("header", Size::new(80, 1)).interactive(true));
    let sidebar = Box::new(RenderLeaf::new("sidebar", Size::new(15, 22)).interactive(true));
    let main_content = Box::new(RenderLeaf::new("main", Size::new(65, 22)).interactive(true));
    let footer = Box::new(RenderLeaf::new("footer", Size::new(80, 1)).interactive(true));

    let body_row = Box::new(
        RenderFlex::row()
            .with_child(FlexChild::fixed(sidebar))
            .with_child(FlexChild::flex(main_content, 1))
    );

    let main_col = Box::new(
        RenderFlex::column()
            .with_child(FlexChild::fixed(header))
            .with_child(FlexChild::flex(body_row, 1))
            .with_child(FlexChild::fixed(footer))
    );

    let stack = Box::new(
        RenderStack::expand()
            .with_child(StackChild::fill(bg))
            .with_child(StackChild::fill(main_col))
    );

    let mut tree = RenderTree::new(stack);
    let size = tree.layout(Size::new(80, 24));
    assert_eq!(size, Size::new(80, 24));

    // Hit-testing inside sidebar area: x=5, y=5
    let hit = tree.hit_test((5, 5));
    assert_eq!(hit.top().map(|e| e.tag), Some("sidebar"));

    // Hit-testing inside main area: x=30, y=5
    let hit_main = tree.hit_test((30, 5));
    assert_eq!(hit_main.top().map(|e| e.tag), Some("main"));

    // Hit-testing footer: x=40, y=23
    let hit_footer = tree.hit_test((40, 23));
    assert_eq!(hit_footer.top().map(|e| e.tag), Some("footer"));
}
