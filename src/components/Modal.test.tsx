// @vitest-environment jsdom
// Regression tests for Modal focus handling (issue #10): a parent re-render that passes a new
// inline onClose must not move focus away from the field the user is typing in.
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { Modal } from "./ui";

afterEach(cleanup);

describe("Modal focus", () => {
  it("keeps focus in a field when the parent re-renders with a new onClose", () => {
    const tree = (onClose: () => void) => (
      <Modal open onClose={onClose} title="T">
        <input aria-label="first" />
        <input aria-label="second" />
      </Modal>
    );
    const { rerender } = render(tree(() => {}));
    const second = screen.getByLabelText("second");
    second.focus();
    expect(document.activeElement).toBe(second);

    rerender(tree(() => {}));

    expect(document.activeElement).toBe(second);
  });

  it("keeps an autoFocus child focused on open", () => {
    render(
      <Modal open onClose={() => {}} title="T">
        <button>Cancel</button>
        <button autoFocus>Confirm</button>
      </Modal>,
    );
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Confirm" }));
  });

  it("focuses the first field on open when nothing inside is focused", () => {
    render(
      <Modal open onClose={() => {}} title="T">
        <input aria-label="first" />
        <input aria-label="second" />
      </Modal>,
    );
    expect(document.activeElement).toBe(screen.getByLabelText("first"));
  });

  it("restores focus to the previously focused element on close", () => {
    const tree = (open: boolean) => (
      <>
        <button>Opener</button>
        <Modal open={open} onClose={() => {}} title="T">
          <input aria-label="first" />
        </Modal>
      </>
    );
    const { rerender } = render(tree(false));
    const opener = screen.getByRole("button", { name: "Opener" });
    opener.focus();

    rerender(tree(true));
    expect(document.activeElement).toBe(screen.getByLabelText("first"));

    rerender(tree(false));
    expect(document.activeElement).toBe(opener);
  });

  it("calls the latest onClose on Escape", () => {
    const a = vi.fn();
    const b = vi.fn();
    const tree = (onClose: () => void) => (
      <Modal open onClose={onClose} title="T">
        <input aria-label="first" />
      </Modal>
    );
    const { rerender } = render(tree(a));
    rerender(tree(b));

    fireEvent.keyDown(document, { key: "Escape" });

    expect(b).toHaveBeenCalledTimes(1);
    expect(a).not.toHaveBeenCalled();
  });
});
