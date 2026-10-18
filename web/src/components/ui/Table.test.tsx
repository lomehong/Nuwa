import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { Table, TableBody, TableCell, TableHead, TableHeaderCell, TableRow } from "./Table";

describe("Table", () => {
  it("is a semantic table in a labelled, keyboard-scrollable region", () => {
    render(
      <Table caption="问题单列表">
        <TableHead>
          <TableRow>
            <TableHeaderCell>标题</TableHeaderCell>
            <TableHeaderCell>状态</TableHeaderCell>
          </TableRow>
        </TableHead>
        <TableBody>
          <TableRow>
            <TableCell>心跳丢失</TableCell>
            <TableCell>修复中</TableCell>
          </TableRow>
        </TableBody>
      </Table>,
    );

    const region = screen.getByRole("region", { name: "问题单列表" });
    expect(region).toHaveAttribute("tabindex", "0");
    expect(screen.getByRole("table", { name: "问题单列表" })).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "标题" })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "心跳丢失" })).toBeInTheDocument();
  });
});
