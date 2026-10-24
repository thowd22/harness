"""Plain-text stock reports."""


def format_cents(cents):
    return f"${cents // 100}.{cents % 100:02d}"


def format_report(stock):
    lines = []
    for sku in stock.skus():
        lines.append(f"{sku}: {stock.quantity(sku)}")
    lines.append(f"total: {format_cents(stock.total_value_cents())}")
    return "\n".join(lines)
